//! The sessions the brick keeps on disk, one JSON file each, so that what
//! the transcripts do not hold outlives the service: see [`crate::state`] for
//! what a file holds.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where the files go, under the state directory.
const SUBDIR: &str = "nowcast/bricks/claude-code/sessions";

/// The directory of the files: under `$XDG_STATE_HOME` when it is absolute,
/// else under `$HOME/.local/state`; `None` without either.
pub fn dir(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_state_home.map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => PathBuf::from(home?).join(".local/state"),
    };
    Some(base.join(SUBDIR))
}

/// Whether a session id can name a file: a client chooses it, so nothing
/// that could leave the directory.
pub fn storable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Every file of the directory, by session id: its JSON, or why it cannot
/// be read. A directory that does not exist holds none.
pub fn load(dir: &Path) -> io::Result<Vec<(String, Result<Value, String>)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|id| storable(id))
        else {
            continue;
        };
        let read = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()));
        out.push((id.to_owned(), read));
    }
    Ok(out)
}

/// Writes a session's file whole: to a temporary file first, renamed over
/// the old one, so that a stop leaves one version or the other.
pub fn save(dir: &Path, id: &str, value: &Value) -> io::Result<()> {
    if !storable(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("session id {id:?} cannot name a file"),
        ));
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{id}.json.tmp"));
    std::fs::write(&tmp, value.to_string())?;
    std::fs::rename(&tmp, dir.join(format!("{id}.json")))
}

/// Removes a session's file; one already gone is not an error.
pub fn remove(dir: &Path, id: &str) -> io::Result<()> {
    if !storable(id) {
        return Ok(());
    }
    match std::fs::remove_file(dir.join(format!("{id}.json"))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_directory_follows_xdg_state_home_when_absolute() {
        let home = Some(OsString::from("/home/u"));
        assert_eq!(
            dir(Some("/state".into()), home.clone()),
            Some(PathBuf::from("/state/nowcast/bricks/claude-code/sessions"))
        );
        assert_eq!(
            dir(Some("relative".into()), home.clone()),
            Some(PathBuf::from(
                "/home/u/.local/state/nowcast/bricks/claude-code/sessions"
            ))
        );
        assert_eq!(dir(None, None), None);
    }

    #[test]
    fn only_plain_ids_name_a_file() {
        assert!(storable("8e730a94-48e5-417c-b4fc-b848b95cabfb"));
        for id in ["", "../x", "a/b", ".hidden", "a.json"] {
            assert!(!storable(id), "{id}");
        }
    }

    #[test]
    fn a_saved_session_loads_back_and_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        assert!(load(&dir).unwrap().is_empty());

        save(&dir, "s1", &json!({ "a": 1 })).unwrap();
        save(&dir, "s1", &json!({ "a": 2 })).unwrap();
        std::fs::write(dir.join("s2.json"), "{ half").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let mut loaded = load(&dir).unwrap();
        loaded.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(loaded[0], ("s1".into(), Ok(json!({ "a": 2 }))));
        assert_eq!(loaded[1].0, "s2");
        assert!(loaded[1].1.is_err());
        assert_eq!(loaded.len(), 2);

        remove(&dir, "s1").unwrap();
        remove(&dir, "s1").unwrap();
        assert!(!dir.join("s1.json").exists());
        assert!(save(&dir, "../escape", &json!({})).is_err());
    }
}
