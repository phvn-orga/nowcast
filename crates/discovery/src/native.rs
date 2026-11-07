//! Native bricks: the bricks the `nowcast` binary embeds, declaration and
//! schemas included. Activating one writes those files where a link can point
//! at them, then links them like any brick (contract section 2).

use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use nowcast_types::BrickName;

use crate::manifest::{ManifestError, load_manifest};

/// The name of a native brick's declaration file, among its files.
pub const DECLARATION: &str = "brick.toml";

/// The files of a native brick, embedded in the binary: paths relative to
/// the brick's directory, [`DECLARATION`] among them.
#[derive(Debug, Clone, Copy)]
pub struct NativeFiles {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

/// Writes the files of `brick` under `<dir>/<name>/`, each one only when its
/// content differs, through a rename so that no reader sees half a file.
/// Returns the path of its declaration.
pub fn write_native(dir: &Path, brick: &NativeFiles) -> io::Result<PathBuf> {
    let root = dir.join(brick.name);
    for (path, contents) in brick.files {
        let target = root.join(path);
        if std::fs::read(&target).is_ok_and(|current| current == contents.as_bytes()) {
            continue;
        }
        let parent = target.parent().unwrap_or(&root);
        std::fs::create_dir_all(parent)?;
        let partial = parent.join(format!(".{}.partial", std::process::id()));
        std::fs::write(&partial, contents)?;
        std::fs::rename(&partial, &target)?;
    }
    Ok(root.join(DECLARATION))
}

/// Rewrites the native bricks written once under `dir`, those whose directory
/// exists, so that their files match this binary. A brick that cannot be
/// rewritten is logged and left as is.
pub fn refresh_natives(dir: &Path, bricks: &[NativeFiles]) {
    for brick in bricks {
        if !dir.join(brick.name).is_dir() {
            continue;
        }
        if let Err(e) = write_native(dir, brick) {
            tracing::warn!(brick = brick.name, "cannot rewrite the native brick: {e}");
        }
    }
}

/// Why a brick could not be enabled.
#[derive(Debug, thiserror::Error)]
pub enum EnableError {
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("{path} exists and is not a link: remove it first")]
    NotALink { path: PathBuf },
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Activates the declaration file at `declaration`: checks it loads, then
/// links `<bricks>/<name>.toml` to it, replacing an earlier link of that name.
pub fn enable(bricks: &Path, declaration: &Path) -> Result<BrickName, EnableError> {
    let loaded = load_manifest(declaration)?;
    let target = declaration.canonicalize()?;
    let link = bricks.join(format!("{}.toml", loaded.name));
    std::fs::create_dir_all(bricks)?;
    match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&link)?,
        Ok(_) => return Err(EnableError::NotALink { path: link }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    symlink(&target, &link)?;
    Ok(loaded.name)
}

/// Removes the link `<bricks>/<name>.toml`. Returns whether there was one; a
/// file there that is not a link is left alone and refused.
pub fn disable(bricks: &Path, name: &BrickName) -> Result<bool, EnableError> {
    let link = bricks.join(format!("{name}.toml"));
    match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::remove_file(&link)?;
            Ok(true)
        }
        Ok(_) => Err(EnableError::NotALink { path: link }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"{ "type": "object" }"#;
    const DECL: &str = r#"manifest_version = 1
name = "native"
version = "0.1.0"
description = "A native brick."
protocol = 1

[run]
command = ["nowcast", "brick", "native"]

[subscriptions.one]
description = "One."
schema = "schemas/one.json"
publishes = "native.one/v1"

[types."native.one/v1"]
schema = "schemas/one.json"
"#;
    const NATIVE: NativeFiles = NativeFiles {
        name: "native",
        files: &[(DECLARATION, DECL), ("schemas/one.json", SCHEMA)],
    };

    #[test]
    fn enabling_writes_the_files_and_links_the_declaration() {
        let root = tempfile::tempdir().unwrap();
        let (data, bricks) = (root.path().join("data"), root.path().join("bricks"));
        let declaration = write_native(&data, &NATIVE).unwrap();
        assert_eq!(declaration, data.join("native/brick.toml"));
        assert_eq!(
            std::fs::read_to_string(data.join("native/schemas/one.json")).unwrap(),
            SCHEMA
        );
        let name = enable(&bricks, &declaration).unwrap();
        assert_eq!(name.as_str(), "native");
        let link = bricks.join("native.toml");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            declaration.canonicalize().unwrap()
        );
        // Enabling again replaces the link.
        enable(&bricks, &declaration).unwrap();
        assert!(disable(&bricks, &name).unwrap());
        assert!(!link.exists());
        assert!(!disable(&bricks, &name).unwrap());
    }

    #[test]
    fn a_file_in_place_of_the_link_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (data, bricks) = (root.path().join("data"), root.path().join("bricks"));
        let declaration = write_native(&data, &NATIVE).unwrap();
        std::fs::create_dir_all(&bricks).unwrap();
        std::fs::write(bricks.join("native.toml"), "mine").unwrap();
        assert!(matches!(
            enable(&bricks, &declaration),
            Err(EnableError::NotALink { .. })
        ));
        let name = BrickName::new("native").unwrap();
        assert!(matches!(
            disable(&bricks, &name),
            Err(EnableError::NotALink { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(bricks.join("native.toml")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn refreshing_rewrites_only_bricks_written_once() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        refresh_natives(&data, &[NATIVE]);
        assert!(!data.join("native").exists());
        write_native(&data, &NATIVE).unwrap();
        std::fs::write(data.join("native/schemas/one.json"), "{}").unwrap();
        refresh_natives(&data, &[NATIVE]);
        assert_eq!(
            std::fs::read_to_string(data.join("native/schemas/one.json")).unwrap(),
            SCHEMA
        );
    }
}
