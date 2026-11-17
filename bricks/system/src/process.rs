//! The cost of a process and of everything it started, read from `/proc`, on
//! the scale of the machine flow: process count, anonymous memory, and the CPU
//! time taken between two readings, in percent of all cores and of one.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nowcast_types::AnchorProcess;
use serde_json::{Value, json};

const MIB: u64 = 1024 * 1024;

/// How many readings a process that disappeared waits for its time to show
/// in an ancestor's `cutime`: past that, it left the tree.
const PENDING_READINGS: u32 = 2;

/// A process for the lifetime of the system: a pid with its start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    pid: u32,
    start: u64,
}

/// What `/proc/<pid>/stat` says of one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    ppid: u32,
    /// Clock ticks since boot when the process started.
    start: u64,
    /// `utime + stime`, in clock ticks.
    own: u64,
    /// `cutime + cstime`: the time of the children it waited for, theirs
    /// included.
    reaped: u64,
}

/// Parses a `stat` line. The name, field 2, can hold spaces and
/// parentheses: the fields that follow start after its last `)`.
fn parse(line: &str) -> Option<Stat> {
    let rest = line.get(line.rfind(')')? + 2..)?;
    // Field 3 (state) is the first one here, so field n is at n - 3.
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some(Stat {
        ppid: u32::try_from(field(4)?).ok()?,
        start: field(22)?,
        own: field(14)? + field(15)?,
        reaped: field(16)? + field(17)?,
    })
}

/// Anonymous resident pages from a `statm` line: `resident` minus `shared`,
/// the file and shared-memory pages.
fn anonymous(line: &str) -> Option<u64> {
    let mut fields = line.split_ascii_whitespace().skip(1);
    let resident: u64 = fields.next()?.parse().ok()?;
    let shared: u64 = fields.next()?.parse().ok()?;
    Some(resident.saturating_sub(shared))
}

/// The system's clock ticks per second, page size and online CPUs.
#[derive(Debug, Clone, Copy)]
pub struct Units {
    pub ticks_per_second: u64,
    pub page_size: u64,
    pub cpus: u64,
}

impl Units {
    pub fn of_system() -> Self {
        let read = |name, default: u64| {
            // SAFETY: `sysconf` reads a system value and has no precondition.
            let value = unsafe { libc::sysconf(name) };
            u64::try_from(value)
                .ok()
                .filter(|&v| v > 0)
                .unwrap_or(default)
        };
        Self {
            ticks_per_second: read(libc::_SC_CLK_TCK, 100),
            page_size: read(libc::_SC_PAGESIZE, 4096),
            cpus: read(libc::_SC_NPROCESSORS_ONLN, 1),
        }
    }
}

/// A process of the tree at one reading.
#[derive(Debug, Clone, Copy)]
struct Seen {
    /// Its parent, `None` for the root.
    parent: Option<Key>,
    own: u64,
    reaped: u64,
}

/// A process gone since an earlier reading, whose time already counted.
#[derive(Debug, Clone, Copy)]
struct Pending {
    parent: Option<Key>,
    /// Its `own + reaped` when last seen, not yet found again in an ancestor's
    /// `reaped`.
    counted: u64,
    readings: u32,
}

/// One process tree, followed from its root between readings.
pub struct Tracker {
    proc: PathBuf,
    anchor: AnchorProcess,
    units: Units,
    /// Once the root is gone, or its pid names another process.
    ended: bool,
    previous: Option<(Instant, HashMap<Key, Seen>)>,
    pending: HashMap<Key, Pending>,
}

impl Tracker {
    /// Follows the tree of `anchor`, whose `/proc` is `root/proc`.
    pub fn new(root: &Path, anchor: AnchorProcess, units: Units) -> Self {
        Self {
            proc: root.join("proc"),
            anchor,
            units,
            ended: false,
            previous: None,
            pending: HashMap::new(),
        }
    }

    /// Reads the tree. The detail once a previous reading gives a CPU usage;
    /// `None` on the first reading, and for good once the process is gone or
    /// its pid names another process.
    pub fn read(&mut self, now: Instant) -> Option<Value> {
        if self.ended {
            return None;
        }
        let Some((seen, anon_pages)) = self.tree() else {
            self.ended = true;
            return None;
        };
        let previous = self.previous.replace((now, seen.clone()));
        let (at, before) = previous?;
        let spent = self.spent(&before, &seen);
        let elapsed = now.duration_since(at).as_secs_f64();
        let one_core = if elapsed > 0.0 {
            spent as f64 / self.units.ticks_per_second as f64 / elapsed * 100.0
        } else {
            0.0
        };
        Some(json!({
            "pid": self.anchor.pid,
            "processes": seen.len(),
            "cpu": {
                "usage": round2(one_core / self.units.cpus as f64),
                "usage_one_core": round2(one_core),
            },
            "memory": {
                "anon_mib": (anon_pages * self.units.page_size + MIB / 2) / MIB,
            },
        }))
    }

    /// The clock ticks the tree took between `before` and `now`. Each process
    /// counts its own difference, a new one all its time. A process gone
    /// since `before` waits, with what already counted of it, for that time to
    /// show again in the `reaped` of its nearest ancestor still there, which
    /// it is then taken from.
    fn spent(&mut self, before: &HashMap<Key, Seen>, now: &HashMap<Key, Seen>) -> u64 {
        for (key, seen) in before {
            if !now.contains_key(key) {
                self.pending.insert(
                    *key,
                    Pending {
                        parent: seen.parent,
                        counted: seen.own + seen.reaped,
                        readings: 0,
                    },
                );
            }
        }
        let mut spent = 0;
        let mut jumps = HashMap::new();
        for (key, seen) in now {
            if let Some(old) = before.get(key) {
                spent += seen.own.saturating_sub(old.own);
                jumps.insert(*key, seen.reaped.saturating_sub(old.reaped));
            } else if let Some(missed) = self.pending.remove(key) {
                // Missed by a reading, not gone: it resumes where it was.
                spent += (seen.own + seen.reaped).saturating_sub(missed.counted);
            } else {
                spent += seen.own + seen.reaped;
            }
        }
        // What the gone processes already counted comes off their ancestors'
        // jumps, as far as each jump goes.
        let keys: Vec<Key> = self.pending.keys().copied().collect();
        for key in keys {
            let Some(ancestor) = self.present_ancestor(key, now) else {
                continue;
            };
            let (Some(jump), Some(pending)) =
                (jumps.get_mut(&ancestor), self.pending.get_mut(&key))
            else {
                continue;
            };
            let taken = (*jump).min(pending.counted);
            *jump -= taken;
            pending.counted -= taken;
        }
        spent += jumps.values().sum::<u64>();
        self.pending.retain(|_, pending| {
            pending.readings += 1;
            pending.counted > 0 && pending.readings < PENDING_READINGS
        });
        spent
    }

    /// The nearest ancestor of the pending process `key` still in `now`.
    fn present_ancestor(&self, key: Key, now: &HashMap<Key, Seen>) -> Option<Key> {
        let mut parent = self.pending.get(&key)?.parent;
        // Each step goes up one generation, among processes gone: no loop.
        for _ in 0..=self.pending.len() {
            let candidate = parent?;
            if now.contains_key(&candidate) {
                return Some(candidate);
            }
            parent = self.pending.get(&candidate)?.parent;
        }
        None
    }

    /// The tree's processes and their anonymous pages, `None` when the root
    /// is gone or is not the anchor.
    fn tree(&self) -> Option<(HashMap<Key, Seen>, u64)> {
        let root = stat(&self.proc, self.anchor.pid)?;
        if root.start != self.anchor.start {
            return None;
        }
        // The kernel has `children` files only with `CONFIG_PROC_CHILDREN`.
        let main = self
            .proc
            .join(format!("{0}/task/{0}/children", self.anchor.pid));
        let pids = if main.exists() {
            walk(&self.proc, self.anchor.pid)
        } else {
            scan(&self.proc, self.anchor.pid)
        };
        let mut stats: Vec<(u32, Stat)> = vec![(self.anchor.pid, root)];
        stats.extend(
            pids.into_iter()
                .skip(1)
                .filter_map(|pid| Some((pid, stat(&self.proc, pid)?))),
        );
        let keys: HashMap<u32, Key> = stats
            .iter()
            .map(|(pid, s)| {
                let key = Key {
                    pid: *pid,
                    start: s.start,
                };
                (*pid, key)
            })
            .collect();
        let mut anon_pages = 0;
        let mut seen = HashMap::with_capacity(stats.len());
        for (i, (pid, s)) in stats.iter().enumerate() {
            anon_pages += fs::read_to_string(self.proc.join(format!("{pid}/statm")))
                .ok()
                .and_then(|line| anonymous(&line))
                .unwrap_or(0);
            let parent = (i > 0).then(|| keys.get(&s.ppid).copied()).flatten();
            seen.insert(
                keys[pid],
                Seen {
                    parent,
                    own: s.own,
                    reaped: s.reaped,
                },
            );
        }
        Some((seen, anon_pages))
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn stat(proc: &Path, pid: u32) -> Option<Stat> {
    parse(&fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?)
}

/// The children of every thread of `pid`, from the `children` files: a
/// child is listed under the thread that started it.
fn children_of(proc: &Path, pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let Ok(tasks) = fs::read_dir(proc.join(format!("{pid}/task"))) else {
        return children;
    };
    for task in tasks.flatten() {
        if let Ok(list) = fs::read_to_string(task.path().join("children")) {
            children.extend(
                list.split_ascii_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok()),
            );
        }
    }
    children
}

/// `pid` and its descendants, breadth first, each process's children given
/// by `children`.
fn descendants(pid: u32, mut children: impl FnMut(u32) -> Vec<u32>) -> Vec<u32> {
    let mut pids = vec![pid];
    let mut i = 0;
    while let Some(&next) = pids.get(i) {
        pids.extend(children(next));
        i += 1;
    }
    pids
}

/// `pid` and its descendants, through the `children` files.
fn walk(proc: &Path, pid: u32) -> Vec<u32> {
    descendants(pid, |next| children_of(proc, next))
}

/// `pid` and its descendants, from every process's parent.
fn scan(proc: &Path, pid: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = fs::read_dir(proc) {
        for entry in entries.flatten() {
            let Some(child) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
                continue;
            };
            if let Some(s) = stat(proc, child) {
                children.entry(s.ppid).or_default().push(child);
            }
        }
    }
    descendants(pid, |next| children.remove(&next).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::testing::Tree;

    const UNITS: Units = Units {
        ticks_per_second: 100,
        page_size: 4096,
        cpus: 4,
    };

    /// A `stat` line with `own` and `reaped` ticks.
    fn line(pid: u32, name: &str, ppid: u32, start: u64, own: u64, reaped: u64) -> String {
        format!(
            "{pid} ({name}) S {ppid} {pid} {pid} 0 -1 4194560 100 0 0 0 {own} 0 {reaped} 0 20 0 4 0 {start} 1000000 77 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0"
        )
    }

    fn start_of(pid: u32) -> u64 {
        5000 + u64::from(pid)
    }

    fn anchor(pid: u32) -> AnchorProcess {
        AnchorProcess {
            pid,
            start: start_of(pid),
        }
    }

    /// Process `pid` with `own` and `reaped` ticks, `anon` anonymous pages
    /// out of `anon + 100` resident, and `children`.
    fn process(
        tree: &Tree,
        pid: u32,
        ppid: u32,
        own: u64,
        reaped: u64,
        anon: u64,
        children: &[u32],
    ) {
        tree.file(
            &format!("proc/{pid}/stat"),
            &line(pid, "a (b) c", ppid, start_of(pid), own, reaped),
        );
        tree.file(
            &format!("proc/{pid}/statm"),
            &format!("9999 {} 100 1 0 50 0\n", anon + 100),
        );
        children_file(tree, pid, children);
    }

    fn children_file(tree: &Tree, pid: u32, children: &[u32]) {
        let list: Vec<String> = children.iter().map(u32::to_string).collect();
        tree.file(
            &format!("proc/{pid}/task/{pid}/children"),
            &format!("{} ", list.join(" ")),
        );
    }

    fn gone(tree: &Tree, pid: u32) {
        fs::remove_dir_all(tree.root().join(format!("proc/{pid}"))).unwrap();
    }

    /// Readings one second apart; the first gives nothing.
    struct Clock(Instant);

    impl Clock {
        fn start(tracker: &mut Tracker) -> Self {
            let clock = Self(Instant::now());
            assert_eq!(
                tracker.read(clock.0),
                None,
                "the first reading gives no usage"
            );
            clock
        }

        fn read(&mut self, tracker: &mut Tracker) -> Option<Value> {
            self.0 += Duration::from_secs(1);
            tracker.read(self.0)
        }

        /// `usage_one_core` of the next reading: ticks per second, at 100
        /// ticks per second.
        fn ticks(&mut self, tracker: &mut Tracker) -> f64 {
            self.read(tracker).unwrap()["cpu"]["usage_one_core"]
                .as_f64()
                .unwrap()
        }
    }

    #[test]
    fn a_stat_line_parses_past_a_name_with_spaces_and_parentheses() {
        let stat = parse(&line(7, "a (b) c", 1, 42, 3, 4)).unwrap();
        assert_eq!(
            stat,
            Stat {
                ppid: 1,
                start: 42,
                own: 3,
                reaped: 4
            }
        );
        assert_eq!(parse("7 (cut"), None);
        assert_eq!(anonymous("10 8 3 1 0 2 0"), Some(5));
    }

    #[test]
    fn the_tree_counts_descendants_on_the_machine_scale() {
        let tree = Tree::new();
        process(&tree, 10, 1, 100, 0, 256, &[11]);
        process(&tree, 11, 10, 0, 0, 512, &[12]);
        process(&tree, 12, 11, 0, 0, 256, &[]);
        // Unrelated: not counted.
        process(&tree, 20, 1, 900, 0, 9999, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        // In 1 s: 50 ticks of its own, 100 of 12.
        process(&tree, 10, 1, 150, 0, 256, &[11]);
        process(&tree, 12, 11, 100, 0, 256, &[]);
        assert_eq!(
            clock.read(&mut tracker).unwrap(),
            json!({
                "pid": 10,
                "processes": 3,
                "cpu": { "usage": 37.5, "usage_one_core": 150.0 },
                "memory": { "anon_mib": 4 },
            })
        );
    }

    #[test]
    fn a_child_reaped_between_readings_counts_once() {
        let tree = Tree::new();
        process(&tree, 10, 1, 100, 0, 1, &[11]);
        process(&tree, 11, 10, 300, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        // 11 took 40 more ticks, ended, and 10 waited for it.
        gone(&tree, 11);
        process(&tree, 10, 1, 100, 340, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 40.0);
    }

    #[test]
    fn a_whole_command_ending_between_readings_counts_once() {
        let tree = Tree::new();
        // The agent 10 runs a shell 11, which runs a long test binary 12.
        process(&tree, 10, 1, 0, 0, 1, &[11]);
        process(&tree, 11, 10, 10, 0, 1, &[12]);
        process(&tree, 12, 11, 6000, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        // 12 took 20 more and ended, 11 waited for it, took 5 more and ended.
        gone(&tree, 12);
        gone(&tree, 11);
        process(&tree, 10, 1, 0, 6035, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 25.0);
    }

    #[test]
    fn a_child_missing_from_a_reading_counts_once() {
        let tree = Tree::new();
        process(&tree, 10, 1, 0, 0, 1, &[11]);
        process(&tree, 11, 10, 6000, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        // 11 ended after 10 was read: in neither 10's time nor the tree.
        gone(&tree, 11);
        children_file(&tree, 10, &[]);
        assert_eq!(clock.ticks(&mut tracker), 0.0);
        // Then 10 waited for it: only its last 30 ticks are new.
        process(&tree, 10, 1, 0, 6030, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 30.0);
    }

    #[test]
    fn a_process_missed_by_a_reading_resumes_where_it_was() {
        let tree = Tree::new();
        process(&tree, 10, 1, 0, 0, 1, &[11]);
        process(&tree, 11, 10, 6000, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        children_file(&tree, 10, &[]);
        assert_eq!(clock.ticks(&mut tracker), 0.0);
        children_file(&tree, 10, &[11]);
        process(&tree, 11, 10, 6050, 0, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 50.0);
    }

    #[test]
    fn a_process_that_left_the_tree_stops_waiting() {
        let tree = Tree::new();
        process(&tree, 10, 1, 0, 0, 1, &[11]);
        process(&tree, 11, 10, 500, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        // 11 is now init's: 10 never waits for it.
        children_file(&tree, 10, &[]);
        assert_eq!(clock.ticks(&mut tracker), 0.0);
        assert_eq!(clock.ticks(&mut tracker), 0.0);
        // Past the wait, a child 10 waits for counts in full.
        process(&tree, 10, 1, 0, 80, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 80.0);
    }

    #[test]
    fn a_new_process_counts_all_its_time() {
        let tree = Tree::new();
        process(&tree, 10, 1, 0, 0, 1, &[]);
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        process(&tree, 10, 1, 0, 0, 1, &[11]);
        process(&tree, 11, 10, 70, 0, 1, &[]);
        assert_eq!(clock.ticks(&mut tracker), 70.0);
    }

    #[test]
    fn a_pid_naming_another_process_ends_the_readings() {
        let tree = Tree::new();
        process(&tree, 10, 1, 0, 0, 1, &[]);
        let other = AnchorProcess { pid: 10, start: 1 };
        assert_eq!(
            Tracker::new(tree.root(), other, UNITS).read(Instant::now()),
            None
        );
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        tree.file("proc/10/stat", &line(10, "other", 1, 1, 0, 0));
        assert_eq!(clock.read(&mut tracker), None);
        process(&tree, 10, 1, 0, 0, 1, &[]);
        assert_eq!(clock.read(&mut tracker), None, "not back once ended");
    }

    #[test]
    fn a_process_gone_gives_nothing() {
        let tree = Tree::new();
        tree.dir("proc");
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        assert_eq!(tracker.read(Instant::now()), None);
    }

    #[test]
    fn without_children_files_every_process_is_scanned() {
        let tree = Tree::new();
        for (pid, ppid) in [(10, 1), (11, 10), (12, 11), (20, 1)] {
            process(&tree, pid, ppid, 0, 0, 256, &[]);
            fs::remove_dir_all(tree.root().join(format!("proc/{pid}/task"))).unwrap();
        }
        tree.dir("proc/self");
        let mut tracker = Tracker::new(tree.root(), anchor(10), UNITS);
        let mut clock = Clock::start(&mut tracker);
        let detail = clock.read(&mut tracker).unwrap();
        assert_eq!(detail["processes"], 3);
        assert_eq!(detail["memory"]["anon_mib"], 3);
    }

    #[test]
    fn this_process_reads_on_the_real_proc() {
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let me = AnchorProcess {
            pid: std::process::id(),
            start: parse(&stat).unwrap().start,
        };
        let mut tracker = Tracker::new(Path::new("/"), me, Units::of_system());
        let t0 = Instant::now();
        assert_eq!(tracker.read(t0), None);
        let detail = tracker.read(t0 + Duration::from_millis(200)).unwrap();
        assert!(detail["processes"].as_u64().unwrap() >= 1);
        assert!(detail["memory"]["anon_mib"].as_u64().unwrap() >= 1);
        let usage = detail["cpu"]["usage"].as_f64().unwrap();
        let one_core = detail["cpu"]["usage_one_core"].as_f64().unwrap();
        assert!(usage <= one_core);
    }
}
