//! The cost of a process and of everything it started, read from `/proc`:
//! process count, summed resident memory, and CPU time between two readings.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

const MIB: u64 = 1024 * 1024;

/// What `/proc/<pid>/stat` says of one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    ppid: u32,
    /// Clock ticks since boot when the process started: with the pid, it
    /// names the process for good.
    start: u64,
    /// `utime + stime + cutime + cstime`, in clock ticks: its own CPU time
    /// and that of the children it waited for.
    ticks: u64,
    rss_pages: u64,
}

/// Parses a `stat` line. The name, field 2, can hold spaces and
/// parentheses: the fields that follow start after its last `)`.
fn parse(line: &str) -> Option<Stat> {
    let rest = line.get(line.rfind(')')? + 2..)?;
    // Field 3 (state) is the first one here, so field n is at n - 3.
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    let ticks = (14..=17).try_fold(0u64, |sum, n| Some(sum + field(n)?))?;
    Some(Stat {
        ppid: u32::try_from(field(4)?).ok()?,
        start: field(22)?,
        ticks,
        rss_pages: field(24)?,
    })
}

/// The system's clock ticks per second and page size, the units of `stat`.
#[derive(Debug, Clone, Copy)]
pub struct Units {
    pub ticks_per_second: u64,
    pub page_size: u64,
}

impl Units {
    pub fn of_system() -> Self {
        // SAFETY: `sysconf` reads a constant and has no precondition.
        let (ticks, page) = unsafe {
            (
                libc::sysconf(libc::_SC_CLK_TCK),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        Self {
            ticks_per_second: u64::try_from(ticks).ok().filter(|&t| t > 0).unwrap_or(100),
            page_size: u64::try_from(page).ok().filter(|&p| p > 0).unwrap_or(4096),
        }
    }
}

/// One process tree, followed from its root's pid between readings.
pub struct Tracker {
    proc: PathBuf,
    pid: u32,
    units: Units,
    /// The root's start time when it was first read; `None` if it was gone.
    start: Option<u64>,
    /// The tree's CPU ticks at the previous reading, and when.
    previous: Option<(Instant, u64)>,
}

impl Tracker {
    /// Follows the tree of `pid`, whose `/proc` is `root/proc`, from the
    /// process that has that pid now.
    pub fn new(root: &Path, pid: u32, units: Units) -> Self {
        let proc = root.join("proc");
        let start = stat(&proc, pid).map(|s| s.start);
        Self {
            proc,
            pid,
            units,
            start,
            previous: None,
        }
    }

    /// Reads the tree. The detail once a previous reading gives a CPU usage;
    /// `None` on the first reading, and once the process is gone or its pid
    /// names another process.
    pub fn read(&mut self, now: Instant) -> Option<Value> {
        let start = self.start?;
        let stats = self.tree();
        if stats.first().map(|s| s.start) != Some(start) {
            self.start = None;
            return None;
        }
        let ticks: u64 = stats.iter().map(|s| s.ticks).sum();
        let rss: u64 = stats.iter().map(|s| s.rss_pages).sum::<u64>() * self.units.page_size;
        let previous = self.previous.replace((now, ticks));
        let (at, before) = previous?;
        let elapsed = now.duration_since(at).as_secs_f64();
        // A process that left the tree took its time with it: the sum went
        // back, and the reading counts nothing.
        let spent = ticks.saturating_sub(before) as f64 / self.units.ticks_per_second as f64;
        let usage = if elapsed > 0.0 {
            (spent / elapsed * 100.0).round() as u64
        } else {
            0
        };
        Some(json!({
            "pid": self.pid,
            "processes": stats.len(),
            "cpu": { "usage": usage },
            "memory": { "rss_mib": (rss as f64 / MIB as f64).round() as u64 },
        }))
    }

    /// The root's `stat` first, then each descendant's.
    fn tree(&self) -> Vec<Stat> {
        let Some(root) = stat(&self.proc, self.pid) else {
            return vec![];
        };
        // The kernel has `children` files only with `CONFIG_PROC_CHILDREN`.
        let main = self.proc.join(format!("{0}/task/{0}/children", self.pid));
        let pids = if main.exists() {
            walk(&self.proc, self.pid)
        } else {
            scan(&self.proc, self.pid)
        };
        let mut stats = vec![root];
        stats.extend(
            pids.into_iter()
                .skip(1)
                .filter_map(|pid| stat(&self.proc, pid)),
        );
        stats
    }
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
    };

    /// A `stat` line: `ticks` split over the four CPU fields, `pages` of RSS.
    fn line(pid: u32, name: &str, ppid: u32, start: u64, ticks: [u64; 4], pages: u64) -> String {
        let [u, s, cu, cs] = ticks;
        format!(
            "{pid} ({name}) S {ppid} {pid} {pid} 0 -1 4194560 100 0 0 0 {u} {s} {cu} {cs} 20 0 4 0 {start} 1000000 {pages} 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0"
        )
    }

    fn process(tree: &Tree, pid: u32, ppid: u32, ticks: [u64; 4], pages: u64, children: &[u32]) {
        tree.file(
            &format!("proc/{pid}/stat"),
            &line(pid, "a (b) c", ppid, 5000 + u64::from(pid), ticks, pages),
        );
        let list: Vec<String> = children.iter().map(u32::to_string).collect();
        tree.file(
            &format!("proc/{pid}/task/{pid}/children"),
            &format!("{} ", list.join(" ")),
        );
    }

    #[test]
    fn a_stat_line_parses_past_a_name_with_spaces_and_parentheses() {
        let stat = parse(&line(7, "a (b) c", 1, 42, [1, 2, 3, 4], 9)).unwrap();
        assert_eq!(
            stat,
            Stat {
                ppid: 1,
                start: 42,
                ticks: 10,
                rss_pages: 9
            }
        );
        assert_eq!(parse("7 (cut"), None);
    }

    #[test]
    fn the_tree_counts_descendants_memory_and_reaped_children() {
        let tree = Tree::new();
        process(&tree, 10, 1, [100, 0, 0, 0], 256, &[11]);
        process(&tree, 11, 10, [0, 0, 0, 0], 512, &[12]);
        process(&tree, 12, 11, [0, 0, 0, 0], 256, &[]);
        // Unrelated: not counted.
        process(&tree, 20, 1, [900, 0, 0, 0], 9999, &[]);
        let mut tracker = Tracker::new(tree.root(), 10, UNITS);
        let t0 = Instant::now();
        assert_eq!(tracker.read(t0), None, "the first reading gives no usage");
        // In 2 s: 50 ticks of its own, 150 of a child that ended and was
        // waited for by 11.
        process(&tree, 10, 1, [150, 0, 0, 0], 256, &[11]);
        process(&tree, 11, 10, [0, 0, 100, 50], 512, &[12]);
        let detail = tracker.read(t0 + Duration::from_secs(2)).unwrap();
        assert_eq!(
            detail,
            json!({ "pid": 10, "processes": 3, "cpu": { "usage": 100 }, "memory": { "rss_mib": 4 } })
        );
    }

    #[test]
    fn time_that_leaves_the_tree_counts_nothing() {
        let tree = Tree::new();
        process(&tree, 10, 1, [100, 0, 0, 0], 1, &[11]);
        process(&tree, 11, 10, [500, 0, 0, 0], 1, &[]);
        let mut tracker = Tracker::new(tree.root(), 10, UNITS);
        let t0 = Instant::now();
        tracker.read(t0);
        process(&tree, 10, 1, [110, 0, 0, 0], 1, &[]);
        let detail = tracker.read(t0 + Duration::from_secs(1)).unwrap();
        assert_eq!(detail["cpu"]["usage"], 0);
        assert_eq!(detail["processes"], 1);
    }

    #[test]
    fn a_pid_naming_another_process_ends_the_readings() {
        let tree = Tree::new();
        process(&tree, 10, 1, [0; 4], 1, &[]);
        let mut tracker = Tracker::new(tree.root(), 10, UNITS);
        let t0 = Instant::now();
        tracker.read(t0);
        tree.file("proc/10/stat", &line(10, "other", 1, 1, [0; 4], 1));
        assert_eq!(tracker.read(t0 + Duration::from_secs(1)), None);
        process(&tree, 10, 1, [0; 4], 1, &[]);
        assert_eq!(
            tracker.read(t0 + Duration::from_secs(2)),
            None,
            "not back once gone"
        );
    }

    #[test]
    fn a_process_gone_gives_nothing() {
        let tree = Tree::new();
        tree.dir("proc");
        let mut tracker = Tracker::new(tree.root(), 10, UNITS);
        assert_eq!(tracker.read(Instant::now()), None);
    }

    #[test]
    fn without_children_files_every_process_is_scanned() {
        let tree = Tree::new();
        let stat = |pid: u32, ppid: u32| line(pid, "x", ppid, 5000 + u64::from(pid), [0; 4], 256);
        tree.file("proc/10/stat", &stat(10, 1));
        tree.file("proc/11/stat", &stat(11, 10));
        tree.file("proc/12/stat", &stat(12, 11));
        tree.file("proc/20/stat", &stat(20, 1));
        tree.dir("proc/self");
        let mut tracker = Tracker::new(tree.root(), 10, UNITS);
        let t0 = Instant::now();
        tracker.read(t0);
        let detail = tracker.read(t0 + Duration::from_secs(1)).unwrap();
        assert_eq!(detail["processes"], 3);
        assert_eq!(detail["memory"]["rss_mib"], 3);
    }

    #[test]
    fn this_process_reads_on_the_real_proc() {
        let mut tracker = Tracker::new(Path::new("/"), std::process::id(), Units::of_system());
        let t0 = Instant::now();
        assert_eq!(tracker.read(t0), None);
        let detail = tracker.read(t0 + Duration::from_millis(200)).unwrap();
        assert!(detail["processes"].as_u64().unwrap() >= 1);
        assert!(detail["memory"]["rss_mib"].as_u64().unwrap() >= 1);
    }
}
