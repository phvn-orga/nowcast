//! Reading a git remote, in any of its spellings, for providers to recognise
//! their forge in it.

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

/// An SSH remote: scp-like, or an `ssh://` URL (`git+ssh://` included). Only
/// an SSH remote can name an SSH alias.
pub fn is_ssh(remote: &str) -> bool {
    match remote.trim().split_once("://") {
        Some((scheme, _)) => scheme.ends_with("ssh"),
        None => true,
    }
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

    #[test]
    fn every_spelling_gives_host_and_path() {
        for remote in [
            "https://github.com/o/r.git",
            "https://user@GitHub.com/o/r/",
            "git@github.com:o/r.git",
            "ssh://git@github.com:22/o/r.git",
            "  git@github.com:o/r  ",
        ] {
            assert_eq!(
                split(remote),
                Some(("github.com".into(), "o/r".into())),
                "{remote}"
            );
        }
        for remote in ["", "not a remote", "/srv/git/repo.git"] {
            assert_eq!(split(remote), None, "{remote}");
        }
        assert!(is_ssh("git@host:o/r") && is_ssh("git+ssh://host/o/r"));
        assert!(!is_ssh("https://host/o/r"));
    }
}
