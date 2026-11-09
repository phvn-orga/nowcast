//! The bricks native to nowcast, compiled into this binary. Adding one is a
//! crate under `bricks/` and an entry here.

use std::process::ExitCode;

use nowcast_discovery::NativeFiles;

pub struct Native {
    pub files: NativeFiles,
    /// Runs the brick on standard input and output, as `nowcast brick <name>`.
    pub run: fn() -> ExitCode,
}

pub const NATIVES: &[Native] = &[
    Native {
        files: NativeFiles {
            name: nowcast_brick_claude_code::NAME,
            files: nowcast_brick_claude_code::FILES,
        },
        run: nowcast_brick_claude_code::run,
    },
    Native {
        files: NativeFiles {
            name: nowcast_brick_forge::NAME,
            files: nowcast_brick_forge::FILES,
        },
        run: nowcast_brick_forge::run,
    },
];

pub fn find(name: &str) -> Option<&'static Native> {
    NATIVES.iter().find(|n| n.files.name == name)
}

/// The files of every native brick, for the daemon to keep them current.
pub fn files() -> Vec<NativeFiles> {
    NATIVES.iter().map(|n| n.files).collect()
}

#[cfg(test)]
mod tests {
    use nowcast_discovery::{DECLARATION, load_manifest, write_native};

    use super::*;

    /// Each native brick's declaration loads, names the brick, is launched by
    /// `nowcast brick <name>` and has the binary's version.
    #[test]
    fn every_native_declaration_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        for native in NATIVES {
            assert!(
                native
                    .files
                    .files
                    .iter()
                    .any(|(path, _)| *path == DECLARATION)
            );
            let path = write_native(dir.path(), &native.files).unwrap();
            let declaration = load_manifest(&path).unwrap();
            assert_eq!(declaration.name.as_str(), native.files.name);
            assert_eq!(declaration.command, ["nowcast", "brick", native.files.name]);
            assert_eq!(
                declaration.version,
                env!("CARGO_PKG_VERSION"),
                "{}",
                native.files.name
            );
        }
    }
}
