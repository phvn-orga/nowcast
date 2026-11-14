//! Whether a push or a PR's head starts a CI on GitHub, from the triggers
//! of the workflows at that commit: for a push, those of the pushed commit;
//! for a PR, those of its head, as an approximation of its merge commit.
//!
//! The workflow directory is listed at the commit, and the repository's
//! workflows give which are disabled; both requests are conditional. Each
//! workflow file is read by its blob, which never changes: its triggers are
//! kept for good ([`Workflows`]), so a new head pays for the files it
//! changed only.
//!
//! The answer ([`Starts`]) is `None` when no active workflow runs on the
//! event for that branch (or there is no workflow directory at all),
//! `Certain` when one runs without a path filter, and `Maybe` for whatever
//! cannot be told: path filters, a `[skip ci]`-style marker in the head
//! commit's message, a PR known to conflict with its base, a trigger not
//! understood, a read that failed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Map, Value};

use super::read::{Facts, HEADERS};
use crate::tools::http::{Failure, Http};
use crate::tools::pace::Starts;

/// The workflow files whose triggers are kept at most; past it, the cache
/// starts again empty.
const KEPT_MAX: usize = 1024;

/// The activity types of a PR that a push on it, or its opening, is.
const PR_ACTIVITIES: &[&str] = &["opened", "synchronize", "reopened"];

/// The markers in a commit message that skip the workflow runs of a push or
/// a PR.
const SKIP_MARKERS: &[&str] = &[
    "[skip ci]",
    "[ci skip]",
    "[no ci]",
    "[skip actions]",
    "[actions skip]",
];

/// What may start a CI.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// A push on `branch`.
    Push { branch: &'a str },
    /// A PR's head pushed, or the PR opened, on `base`.
    PullRequest { base: &'a str },
}

/// The `on` of each workflow file read, by blob: `None` for a file that
/// does not parse, or has no `on`. Shared by every branch of every
/// repository: a blob is the same content wherever it is.
#[derive(Clone, Default)]
pub struct Workflows(Arc<Mutex<HashMap<String, Arc<Option<Value>>>>>);

impl Workflows {
    fn get(&self, blob: &str) -> Option<Arc<Option<Value>>> {
        self.0.lock().unwrap().get(blob).cloned()
    }

    fn keep(&self, blob: &str, on: Arc<Option<Value>>) {
        let mut kept = self.0.lock().unwrap();
        if kept.len() >= KEPT_MAX {
            kept.clear();
        }
        kept.insert(blob.to_owned(), on);
    }
}

/// Where the workflows are read: a repository, with a token.
pub struct Repository<'a> {
    pub http: &'a Http,
    pub api: &'a str,
    pub token: &'a str,
    pub owner: &'a str,
    pub name: &'a str,
}

/// Whether `event` at commit `sha` starts a CI, with what the reading saw of
/// that head (`facts`).
pub async fn starts(
    repo: &Repository<'_>,
    workflows: &Workflows,
    sha: &str,
    event: Event<'_>,
    facts: &Facts,
) -> Starts {
    let base = format!("{}/repos/{}/{}", repo.api, repo.owner, repo.name);
    let get = |url: String| async move { repo.http.get(&url, repo.token, HEADERS).await };
    let (dir, states) = tokio::join!(
        get(format!("{base}/contents/.github/workflows?ref={sha}")),
        get(format!("{base}/actions/workflows?per_page=100")),
    );
    let dir = match dir {
        Ok(dir) => dir,
        Err(Failure::NotFound) => return Starts::None,
        Err(_) => return Starts::Maybe,
    };
    let (Some(files), Ok(states)) = (dir.as_array(), states) else {
        return Starts::Maybe;
    };
    // A workflow file not listed yet is active.
    let disabled: HashSet<&str> = states["workflows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["state"] != "active")
        .filter_map(|w| w["path"].as_str())
        .collect();
    let mut answer = Starts::None;
    for file in files {
        let path = file["path"].as_str().unwrap_or_default();
        if file["type"] != "file"
            || !(path.ends_with(".yml") || path.ends_with(".yaml"))
            || disabled.contains(path)
        {
            continue;
        }
        let on = match file["sha"].as_str() {
            Some(blob) => match workflows.get(blob) {
                Some(on) => Some(on),
                None => match get(format!("{base}/git/blobs/{blob}")).await {
                    Ok(body) => {
                        let on = Arc::new(on_of(&body));
                        workflows.keep(blob, on.clone());
                        Some(on)
                    }
                    Err(_) => None,
                },
            },
            None => None,
        };
        let starts = match on.as_deref() {
            Some(Some(on)) => workflow_starts(on, event),
            _ => Starts::Maybe,
        };
        answer = likelier(answer, starts);
        if answer == Starts::Certain {
            break;
        }
    }
    let skipped = facts.message.as_deref().is_some_and(|m| {
        let m = m.to_lowercase();
        SKIP_MARKERS.iter().any(|marker| m.contains(marker))
    });
    if answer == Starts::Certain && (skipped || facts.conflict) {
        Starts::Maybe
    } else {
        answer
    }
}

/// The `on` of a blob as the API gives it, `None` when it does not decode,
/// does not parse, or has none. YAML 1.2 keeps the key `on` a string.
fn on_of(blob: &Value) -> Option<Value> {
    if blob["encoding"] != "base64" {
        return None;
    }
    let encoded: String = blob["content"]
        .as_str()?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut workflow: Value = serde_saphyr::from_str(&text).ok()?;
    workflow.get_mut("on").map(Value::take)
}

/// Of two answers, the one more likely to start a CI.
fn likelier(a: Starts, b: Starts) -> Starts {
    let rank = |s| match s {
        Starts::None => 0,
        Starts::Maybe => 1,
        Starts::Certain => 2,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// Whether a workflow whose triggers are `on` runs on `event`. `on` is read
/// loosely: an event name, a list of them, or a map from event to its
/// filters; what is not understood is `Maybe`.
fn workflow_starts(on: &Value, event: Event<'_>) -> Starts {
    let names: &[&str] = match event {
        Event::Push { .. } => &["push"],
        Event::PullRequest { .. } => &["pull_request", "pull_request_target"],
    };
    let named = |name: &Value| match name.as_str() {
        Some(name) if names.contains(&name) => Starts::Certain,
        Some(_) => Starts::None,
        None => Starts::Maybe,
    };
    match on {
        Value::String(_) => named(on),
        Value::Array(list) => list.iter().map(named).fold(Starts::None, likelier),
        Value::Object(map) => names
            .iter()
            .filter_map(|name| Some((*name, map.get(*name)?)))
            .map(|(name, filters)| filtered(name, filters, event))
            .fold(Starts::None, likelier),
        _ => Starts::Maybe,
    }
}

/// Whether event `name` with `filters` runs on `event`: a push's branch
/// filters apply to the pushed branch, a PR's to its base; a push filtered
/// on tags only runs on no branch.
fn filtered(name: &str, filters: &Value, event: Event<'_>) -> Starts {
    let filters = match filters {
        Value::Null => return Starts::Certain,
        Value::Object(filters) => filters,
        _ => return Starts::Maybe,
    };
    let branch = match event {
        Event::Push { branch } => branch,
        Event::PullRequest { base } => base,
    };
    if name != "push"
        && let Some(types) = filters.get("types")
    {
        match strings(types) {
            Some(types) if types.iter().any(|t| PR_ACTIVITIES.contains(t)) => {}
            Some(_) => return Starts::None,
            None => return Starts::Maybe,
        }
    }
    let on_branch = match (filters.get("branches"), filters.get("branches-ignore")) {
        (Some(_), Some(_)) => return Starts::Maybe,
        (Some(patterns), None) => match strings(patterns) {
            Some(patterns) => included(&patterns, branch),
            None => return Starts::Maybe,
        },
        (None, Some(patterns)) => match strings(patterns) {
            Some(patterns) => !patterns.iter().any(|p| glob(p, branch)),
            None => return Starts::Maybe,
        },
        (None, None) => !(name == "push" && has_any(filters, &["tags", "tags-ignore"])),
    };
    if !on_branch {
        Starts::None
    } else if has_any(filters, &["paths", "paths-ignore"]) {
        Starts::Maybe
    } else {
        Starts::Certain
    }
}

fn has_any(filters: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|k| filters.contains_key(*k))
}

/// A string, or a list of strings; `None` for anything else.
fn strings(value: &Value) -> Option<Vec<&str>> {
    match value {
        Value::String(s) => Some(vec![s.as_str()]),
        Value::Array(list) => list.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

/// Whether `branches` filter patterns take `name`: the last pattern that
/// matches decides, a `!` one excluding it; none matching excludes it.
fn included(patterns: &[&str], name: &str) -> bool {
    patterns
        .iter()
        .fold(false, |taken, pattern| match pattern.strip_prefix('!') {
            Some(negated) if glob(negated, name) => false,
            Some(_) => taken,
            None => taken || glob(pattern, name),
        })
}

/// One element of a filter pattern.
#[derive(Debug, Clone, PartialEq)]
enum Atom {
    Char(char),
    /// Characters within these inclusive ranges.
    Class(Vec<(char, char)>),
    /// `*`: any characters but `/`.
    Star,
    /// `**`: any characters.
    Stars,
}

/// How many times an atom repeats: `?` after it, zero or one; `+`, one or
/// more.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Times {
    One,
    Optional,
    OneOrMore,
}

/// Whether `pattern`, in GitHub's filter syntax, matches the whole of
/// `name`: `*` any characters but `/`, `**` any characters, `?` zero or
/// one of the preceding character, `+` one or more of it, `[...]` one
/// character listed or in a range, `\` escapes the next character.
fn glob(pattern: &str, name: &str) -> bool {
    let atoms = parse(pattern);
    let name: Vec<char> = name.chars().collect();
    let mut failed = vec![false; (atoms.len() + 1) * (name.len() + 1)];
    matches(&atoms, &name, 0, 0, &mut failed)
}

fn parse(pattern: &str) -> Vec<(Atom, Times)> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut atoms: Vec<(Atom, Times)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let atom = match c {
            '\\' if i < chars.len() => {
                i += 1;
                Atom::Char(chars[i - 1])
            }
            '*' if chars.get(i) == Some(&'*') => {
                i += 1;
                Atom::Stars
            }
            '*' => Atom::Star,
            '?' | '+' if !atoms.is_empty() => {
                let last = atoms.len() - 1;
                atoms[last].1 = if c == '?' {
                    Times::Optional
                } else {
                    Times::OneOrMore
                };
                continue;
            }
            '[' => match class(&chars[i..]) {
                Some((ranges, used)) => {
                    i += used;
                    Atom::Class(ranges)
                }
                None => Atom::Char('['),
            },
            c => Atom::Char(c),
        };
        atoms.push((atom, Times::One));
    }
    atoms
}

/// The ranges of a class whose `[` came just before `chars`, and how many
/// characters it took with its `]`; `None` without a closing `]`.
fn class(chars: &[char]) -> Option<(Vec<(char, char)>, usize)> {
    let end = chars.iter().position(|c| *c == ']')?;
    let inside = &chars[..end];
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < inside.len() {
        if i + 2 < inside.len() && inside[i + 1] == '-' {
            ranges.push((inside[i], inside[i + 2]));
            i += 3;
        } else {
            ranges.push((inside[i], inside[i]));
            i += 1;
        }
    }
    Some((ranges, end + 1))
}

fn one(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Char(a) => *a == c,
        Atom::Class(ranges) => ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)),
        Atom::Star => c != '/',
        Atom::Stars => true,
    }
}

/// Whether `atoms[a..]` match `name[n..]`, remembering in `failed` the
/// positions known not to, so that stars never make it exponential.
fn matches(
    atoms: &[(Atom, Times)],
    name: &[char],
    a: usize,
    n: usize,
    failed: &mut [bool],
) -> bool {
    let at = a * (name.len() + 1) + n;
    if failed[at] {
        return false;
    }
    let Some((atom, times)) = atoms.get(a) else {
        return n == name.len();
    };
    let found = match (atom, times) {
        // A star takes zero or more characters whatever follows it.
        (Atom::Star | Atom::Stars, _) => {
            let mut end = n;
            loop {
                if matches(atoms, name, a + 1, end, failed) {
                    break true;
                }
                if end < name.len() && one(atom, name[end]) {
                    end += 1;
                } else {
                    break false;
                }
            }
        }
        (_, Times::One) => {
            n < name.len() && one(atom, name[n]) && matches(atoms, name, a + 1, n + 1, failed)
        }
        (_, Times::Optional) => {
            (n < name.len() && one(atom, name[n]) && matches(atoms, name, a + 1, n + 1, failed))
                || matches(atoms, name, a + 1, n, failed)
        }
        (_, Times::OneOrMore) => {
            let mut end = n;
            loop {
                if end < name.len() && one(atom, name[end]) {
                    end += 1;
                } else {
                    break false;
                }
                if matches(atoms, name, a + 1, end, failed) {
                    break true;
                }
            }
        }
    };
    if !found {
        failed[at] = true;
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing::Server;

    fn yaml(text: &str) -> Value {
        serde_saphyr::from_str(text).unwrap()
    }

    fn push(on: &str, branch: &str) -> Starts {
        workflow_starts(&yaml(on)["on"], Event::Push { branch })
    }

    fn pr(on: &str, base: &str) -> Starts {
        workflow_starts(&yaml(on)["on"], Event::PullRequest { base })
    }

    #[test]
    fn the_key_on_stays_a_string() {
        let workflow = yaml("on: push\nyes: 1\n");
        assert_eq!(workflow["on"], "push");
        assert_eq!(workflow["yes"], 1);
    }

    #[test]
    fn the_glob_syntax_of_branch_filters() {
        for (pattern, name, expected) in [
            ("main", "main", true),
            ("main", "mainline", false),
            ("feature/*", "feature/my-branch", true),
            ("feature/*", "feature/a/b", false),
            ("feature/**", "feature/a/b", true),
            ("feature/**", "feature/", true),
            ("*", "main", true),
            ("*", "a/b", false),
            ("**", "all/the/branches", true),
            ("*feature", "mona-feature", true),
            ("*feature", "feature", true),
            ("v2*", "v2", true),
            ("v2*", "v2.9", true),
            ("v2*", "v3", false),
            ("v[12].[0-9]+.[0-9]+", "v1.10.1", true),
            ("v[12].[0-9]+.[0-9]+", "v2.0.0", true),
            ("v[12].[0-9]+.[0-9]+", "v3.0.0", false),
            ("v[12].[0-9]+.[0-9]+", "v1..1", false),
            ("[CB]at", "Cat", true),
            ("[CB]at", "Rat", false),
            ("colou?r", "color", true),
            ("colou?r", "colour", true),
            ("colou?r", "colouur", false),
            ("a+b", "aaab", true),
            ("a+b", "b", false),
            ("release\\*", "release*", true),
            ("release\\*", "release1", false),
            ("[ab", "[ab", true),
            ("**/*/**/*/**", "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p", true),
            (
                "*a*a*a*a*a*a*a*a*a*b",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            ),
        ] {
            assert_eq!(glob(pattern, name), expected, "{pattern} on {name}");
        }
    }

    #[test]
    fn negated_patterns_apply_in_order() {
        let patterns = ["releases/**", "!releases/**-alpha", "releases/keep-alpha"];
        assert!(included(&patterns, "releases/10"));
        assert!(!included(&patterns, "releases/10-alpha"));
        assert!(included(&patterns, "releases/keep-alpha"), "taken back");
        assert!(!included(&patterns, "main"));
        assert!(!included(&["!main"], "main"));
        assert!(!included(&[], "main"));
    }

    #[test]
    fn the_triggers_are_read_loosely() {
        assert_eq!(push("on: push", "feat"), Starts::Certain);
        assert_eq!(push("on: [pull_request, push]", "feat"), Starts::Certain);
        assert_eq!(push("on: [pull_request]", "feat"), Starts::None);
        assert_eq!(push("on: [push, 3]", "feat"), Starts::Certain);
        assert_eq!(push("on: [3]", "feat"), Starts::Maybe);
        assert_eq!(push("on: 3", "feat"), Starts::Maybe);
        assert_eq!(
            push("on:\n  push:\n  unknown: x\n", "feat"),
            Starts::Certain
        );
        assert_eq!(push("on:\n  push: x\n", "feat"), Starts::Maybe);
        assert_eq!(
            push("on:\n  push:\n    other: 1\n", "feat"),
            Starts::Certain
        );
        for never in [
            "workflow_dispatch",
            "schedule",
            "workflow_run",
            "workflow_call",
            "repository_dispatch",
        ] {
            let on = format!("on:\n  {never}:\n");
            assert_eq!(push(&on, "main"), Starts::None, "{never}");
            assert_eq!(pr(&on, "main"), Starts::None, "{never}");
        }
    }

    #[test]
    fn a_push_matches_its_branch() {
        let main_only = "on:\n  push:\n    branches: [main]\n";
        assert_eq!(push(main_only, "main"), Starts::Certain);
        assert_eq!(push(main_only, "feat"), Starts::None);
        let one = "on:\n  push:\n    branches: main\n";
        assert_eq!(push(one, "main"), Starts::Certain, "a string");
        let ignored = "on:\n  push:\n    branches-ignore: ['design*']\n";
        assert_eq!(push(ignored, "design-x"), Starts::None);
        assert_eq!(push(ignored, "feat"), Starts::Certain);
        let tags = "on:\n  push:\n    tags: ['v*']\n";
        assert_eq!(push(tags, "main"), Starts::None, "tags only");
        let both = "on:\n  push:\n    tags: ['v*']\n    branches: [main]\n";
        assert_eq!(push(both, "main"), Starts::Certain);
        let paths = "on:\n  push:\n    branches: [main]\n    paths: [Cargo.toml]\n";
        assert_eq!(push(paths, "main"), Starts::Maybe);
        assert_eq!(push(paths, "feat"), Starts::None, "the branch first");
        let ignored_paths = "on:\n  push:\n    paths-ignore: ['docs/**']\n";
        assert_eq!(push(ignored_paths, "feat"), Starts::Maybe);
        let conflicting = "on:\n  push:\n    branches: [main]\n    branches-ignore: [x]\n";
        assert_eq!(push(conflicting, "main"), Starts::Maybe);
        let odd = "on:\n  push:\n    branches: {a: 1}\n";
        assert_eq!(push(odd, "main"), Starts::Maybe);
    }

    #[test]
    fn a_pr_matches_its_base_and_activity() {
        assert_eq!(pr("on: pull_request", "design"), Starts::Certain);
        assert_eq!(pr("on: pull_request_target", "design"), Starts::Certain);
        let to_main = "on:\n  pull_request:\n    branches: [main]\n";
        assert_eq!(pr(to_main, "main"), Starts::Certain);
        assert_eq!(pr(to_main, "design"), Starts::None);
        let closed = "on:\n  pull_request:\n    types: [closed, labeled]\n";
        assert_eq!(pr(closed, "main"), Starts::None);
        let pushed = "on:\n  pull_request:\n    types: [synchronize]\n";
        assert_eq!(pr(pushed, "main"), Starts::Certain);
        let odd = "on:\n  pull_request:\n    types: {a: 1}\n";
        assert_eq!(pr(odd, "main"), Starts::Maybe);
        let tags = "on:\n  pull_request:\n    tags: ['v*']\n";
        assert_eq!(pr(tags, "main"), Starts::Certain, "a PR has no tags");
        assert_eq!(push("on: pull_request", "main"), Starts::None);
    }

    /// The repository's workflows, as nowcast has them.
    const CI: &str =
        "name: CI\non:\n  pull_request:\n  push:\n    branches: [main]\n  workflow_dispatch:\n";
    const AGENTS: &str = "name: Agent instructions\non:\n  pull_request:\n  workflow_dispatch:\n";
    const RELEASE: &str = "name: Release\non:\n  push:\n    branches: [main]\n    paths: [Cargo.toml]\n  workflow_dispatch:\n  pull_request:\n    paths:\n      - Formula/**\n";

    /// A forge whose commits `feat` and `main` hold `files` (path, blob,
    /// content), whose commit `bare` has no workflow directory, and whose
    /// workflows have the `states` given (path, state). `broken` paths
    /// fail with a 500.
    async fn forge(
        files: &'static [(&'static str, &'static str, &'static str)],
        states: &'static [(&'static str, &'static str)],
        broken: &'static [&'static str],
    ) -> Server {
        Server::start(move |request| {
            let line = request.lines().next().unwrap_or_default().to_owned();
            if broken.iter().any(|b| line.contains(b)) {
                return (500, vec![], String::new());
            }
            let body = if line.contains("/contents/.github/workflows?ref=bare") {
                return (404, vec![], String::new());
            } else if line.contains("/contents/.github/workflows?ref=") {
                let mut entries: Vec<Value> = files
                    .iter()
                    .map(|(path, blob, _)| {
                        json!({ "type": "file", "path": format!(".github/workflows/{path}"),
                            "sha": blob })
                    })
                    .collect();
                entries.push(json!({ "type": "dir", "path": ".github/workflows/sub", "sha": "d" }));
                entries.push(
                    json!({ "type": "file", "path": ".github/workflows/README.md",
                    "sha": "m" }),
                );
                json!(entries)
            } else if line.contains("/actions/workflows?per_page=100") {
                let workflows: Vec<Value> = states
                    .iter()
                    .map(|(path, state)| {
                        json!({ "path": format!(".github/workflows/{path}"), "state": state })
                    })
                    .collect();
                json!({ "total_count": workflows.len(), "workflows": workflows })
            } else if let Some((_, _, content)) = files
                .iter()
                .find(|(_, blob, _)| line.contains(&format!("/git/blobs/{blob} ")))
            {
                let encoded = base64::engine::general_purpose::STANDARD.encode(content);
                // The API wraps the content every 60 characters.
                let wrapped: Vec<String> = encoded
                    .as_bytes()
                    .chunks(60)
                    .map(|c| String::from_utf8_lossy(c).into_owned())
                    .collect();
                json!({ "encoding": "base64", "content": wrapped.join("\n") })
            } else {
                return (404, vec![], String::new());
            };
            (200, vec![], body.to_string())
        })
        .await
    }

    const NOWCAST: &[(&str, &str, &str)] = &[
        ("check-agents-md.yml", "b1", AGENTS),
        ("ci.yml", "b2", CI),
        ("release.yml", "b3", RELEASE),
    ];
    const ACTIVE: &[(&str, &str)] = &[
        ("check-agents-md.yml", "active"),
        ("ci.yml", "active"),
        ("release.yml", "active"),
    ];

    async fn ask(server: &Server, workflows: &Workflows, sha: &str, event: Event<'_>) -> Starts {
        ask_with(server, workflows, sha, event, &Facts::default()).await
    }

    async fn ask_with(
        server: &Server,
        workflows: &Workflows,
        sha: &str,
        event: Event<'_>,
        facts: &Facts,
    ) -> Starts {
        let http = Http::new().unwrap();
        let api = server.base();
        let repo = Repository {
            http: &http,
            api: &api,
            token: "t",
            owner: "o",
            name: "r",
        };
        starts(&repo, workflows, sha, event, facts).await
    }

    fn blobs(server: &Server) -> usize {
        server
            .requests()
            .iter()
            .filter(|r| r.contains("/git/blobs/"))
            .count()
    }

    #[tokio::test]
    async fn nowcast_s_workflows_start_a_ci_on_main_and_on_prs_only() {
        let server = forge(NOWCAST, ACTIVE, &[]).await;
        let workflows = Workflows::default();
        let feat = Event::Push { branch: "feat" };
        assert_eq!(ask(&server, &workflows, "feat", feat).await, Starts::None);
        let main = Event::Push { branch: "main" };
        assert_eq!(
            ask(&server, &workflows, "main", main).await,
            Starts::Certain
        );
        let to_main = Event::PullRequest { base: "main" };
        assert_eq!(
            ask(&server, &workflows, "feat", to_main).await,
            Starts::Certain
        );
        let to_design = Event::PullRequest { base: "design" };
        assert_eq!(
            ask(&server, &workflows, "bare", to_design).await,
            Starts::None,
            "no workflow directory"
        );
    }

    /// A first read lists the directory, the workflows and reads each
    /// file; another commit with the same files reads none of them again.
    #[tokio::test]
    async fn a_workflow_file_is_read_once_whatever_the_commit() {
        let server = forge(NOWCAST, ACTIVE, &[]).await;
        let workflows = Workflows::default();
        let feat = Event::Push { branch: "feat" };
        ask(&server, &workflows, "feat", feat).await;
        assert_eq!((server.requests().len(), blobs(&server)), (5, 3));
        let other = Event::Push { branch: "other" };
        ask(&server, &workflows, "other", other).await;
        assert_eq!((server.requests().len(), blobs(&server)), (7, 3));
    }

    #[tokio::test]
    async fn a_path_filtered_workflow_may_start_a_ci() {
        const RELEASE_ONLY: &[(&str, &str, &str)] = &[("release.yml", "b3", RELEASE)];
        let server = forge(RELEASE_ONLY, &[], &[]).await;
        let main = Event::Push { branch: "main" };
        let starts = ask(&server, &Workflows::default(), "main", main).await;
        assert_eq!(starts, Starts::Maybe);
    }

    #[tokio::test]
    async fn a_disabled_workflow_starts_nothing() {
        const DISABLED: &[(&str, &str)] = &[
            ("check-agents-md.yml", "disabled_manually"),
            ("ci.yml", "disabled_inactivity"),
            ("release.yml", "active"),
        ];
        let server = forge(NOWCAST, DISABLED, &[]).await;
        let to_main = Event::PullRequest { base: "main" };
        let starts = ask(&server, &Workflows::default(), "feat", to_main).await;
        assert_eq!(starts, Starts::Maybe, "only the release's path filter");
        assert_eq!(blobs(&server), 1, "a disabled file is not read");
    }

    #[tokio::test]
    async fn a_read_failure_may_start_a_ci() {
        let main = Event::Push { branch: "main" };
        for broken in ["/contents/", "/actions/workflows", "/git/blobs/b2"] {
            let server = forge(NOWCAST, ACTIVE, Box::leak(Box::new([broken]))).await;
            let starts = ask(&server, &Workflows::default(), "main", main).await;
            assert_eq!(starts, Starts::Maybe, "{broken}");
        }
        let server = forge(NOWCAST, ACTIVE, &["/git/blobs/b2"]).await;
        let workflows = Workflows::default();
        ask(&server, &workflows, "main", main).await;
        let fixed = forge(NOWCAST, ACTIVE, &[]).await;
        let starts = ask(&fixed, &workflows, "main", main).await;
        assert_eq!(starts, Starts::Certain, "a failed blob is not kept");
    }

    #[tokio::test]
    async fn a_file_that_does_not_parse_may_start_a_ci() {
        const BROKEN: &[(&str, &str, &str)] = &[("ci.yml", "b9", "on: [push\n")];
        let server = forge(BROKEN, &[], &[]).await;
        let feat = Event::Push { branch: "feat" };
        let starts = ask(&server, &Workflows::default(), "feat", feat).await;
        assert_eq!(starts, Starts::Maybe);
    }

    #[tokio::test]
    async fn a_skip_marker_or_a_conflict_may_start_none() {
        let server = forge(NOWCAST, ACTIVE, &[]).await;
        let workflows = Workflows::default();
        let main = Event::Push { branch: "main" };
        let skip = Facts {
            message: Some("Bump\n\n[Skip CI]".into()),
            conflict: false,
        };
        assert_eq!(
            ask_with(&server, &workflows, "main", main, &skip).await,
            Starts::Maybe
        );
        let conflict = Facts {
            message: None,
            conflict: true,
        };
        let to_main = Event::PullRequest { base: "main" };
        assert_eq!(
            ask_with(&server, &workflows, "feat", to_main, &conflict).await,
            Starts::Maybe
        );
        let feat = Event::Push { branch: "feat" };
        assert_eq!(
            ask_with(&server, &workflows, "feat", feat, &skip).await,
            Starts::None,
            "none stays none"
        );
    }
}
