//! Recognises a git remote, in any of its spellings, and names the
//! repository it designates. Two spellings of one repository give the same
//! [`Repo`], hence the same flow.

/// A repository on a forge the brick reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Repo {
    /// `owner` and `name` in lowercase: GitHub ignores their case.
    GitHub { owner: String, name: String },
}

impl Repo {
    /// The repository's identity in flow ids and details:
    /// `github.com/<owner>/<name>`.
    pub fn id(&self) -> String {
        match self {
            Self::GitHub { owner, name } => format!("github.com/{owner}/{name}"),
        }
    }
}

/// Host and path of a remote: `https://`, `ssh://` or scp-like
/// (`git@host:path`), the host possibly an SSH alias. The host is in
/// lowercase; the path loses its `.git`, and its leading and trailing
/// slashes.
pub fn split(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();
    let (authority, path) = match remote.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => {
            let (authority, path) = remote.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (authority, path)
        }
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next()?.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((host, path.to_owned()))
}

/// The repository of `remote`. On an SSH remote, a host the brick does not
/// know is taken as an SSH alias and handed to `resolve`, which gives its
/// real host name.
pub fn recognise(remote: &str, resolve: impl Fn(&str) -> Option<String>) -> Option<Repo> {
    let (host, path) = split(remote)?;
    let host = if is_github(&host) {
        host
    } else if is_ssh(remote) {
        resolve(&host)?.to_ascii_lowercase()
    } else {
        // Only an SSH remote can name an SSH alias.
        return None;
    };
    if !is_github(&host) {
        return None;
    }
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(Repo::GitHub {
        owner: owner.to_ascii_lowercase(),
        name: name.to_ascii_lowercase(),
    })
}

/// The owner as the remote writes it. GitHub ignores its case, `gh` does
/// not: `gh auth token --user` needs the login's own spelling.
pub fn owner_as_written(remote: &str) -> Option<String> {
    let (_, path) = split(remote)?;
    path.split('/').next().map(str::to_owned)
}

/// An SSH remote: scp-like, or an `ssh://` URL (`git+ssh://` included).
fn is_ssh(remote: &str) -> bool {
    match remote.trim().split_once("://") {
        Some((scheme, _)) => scheme.ends_with("ssh"),
        None => true,
    }
}

fn is_github(host: &str) -> bool {
    host == "github.com" || host == "www.github.com"
}

/// The host name an SSH alias stands for, as `ssh -G` evaluates the user's
/// configuration. `ssh -G` connects to nothing.
pub fn ssh_hostname(alias: &str) -> Option<String> {
    if alias.starts_with('-') {
        return None;
    }
    let output = std::process::Command::new("ssh")
        .args(["-G", alias])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname ").map(|h| h.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn every_spelling_of_a_github_remote_names_one_repo() {
        let repo = Repo::GitHub {
            owner: "phvn-orga".into(),
            name: "nowcast".into(),
        };
        for remote in [
            "https://github.com/phvn-orga/nowcast.git",
            "https://github.com/phvn-orga/nowcast",
            "https://user@github.com/Phvn-Orga/NowCast/",
            "git@github.com:phvn-orga/nowcast.git",
            "ssh://git@github.com:22/phvn-orga/nowcast.git",
            "  git@github.com:phvn-orga/nowcast  ",
        ] {
            assert_eq!(recognise(remote, none), Some(repo.clone()), "{remote}");
        }
        assert_eq!(repo.id(), "github.com/phvn-orga/nowcast");
    }

    #[test]
    fn an_ssh_alias_is_resolved() {
        let resolve = |alias: &str| (alias == "github-pro").then(|| "GitHub.com".to_owned());
        assert_eq!(
            recognise("git@github-pro:org/repo.git", resolve),
            Some(Repo::GitHub {
                owner: "org".into(),
                name: "repo".into()
            })
        );
        assert_eq!(recognise("git@gitlab.com:org/repo.git", resolve), None);
        let never = |_: &str| -> Option<String> { panic!("an HTTPS host is no SSH alias") };
        assert_eq!(recognise("https://github-pro/org/repo.git", never), None);
        assert_eq!(
            owner_as_written("git@github.com:PHVN/Repo.git").as_deref(),
            Some("PHVN")
        );
    }

    #[test]
    fn what_is_not_a_github_repository_is_refused() {
        for remote in [
            "",
            "not a remote",
            "/srv/git/repo.git",
            "https://github.com/only-owner",
            "https://github.com/org/repo/tree/main",
            "https://gitlab.com/org/repo.git",
        ] {
            assert_eq!(recognise(remote, none), None, "{remote}");
        }
    }
}
