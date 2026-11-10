//! `nowcast bricks enable|disable|configure`: activates a brick by linking
//! its declaration into the bricks directory, or removes the link; runs a
//! brick's configuration command in the user's terminal. The service follows
//! the directory; enable only asks it for the `PATH` a brick's program is
//! looked up in.

use std::io::{self, IsTerminal};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nowcast_discovery::{DiscoveryConfig, load_manifest, native_dir_from_env, write_native};
use nowcast_supervisor::SELF;
use nowcast_traits::BrickDeclaration;
use nowcast_types::BrickName;

use crate::client::Client;
use crate::error::CliError;
use crate::native;

/// Enables `brick`: a native brick's name, or the path of any declaration
/// file (one holding a `/` or ending in `.toml`). With `service`, its program
/// is looked for first, and a program missing is refused. Its configuration
/// command runs before the link is made, in a terminal only: one that fails
/// refuses the brick, unless `force`.
pub fn enable(
    brick: &str,
    force: bool,
    service: Option<Result<Client, CliError>>,
) -> Result<(), CliError> {
    let bricks = bricks_dir()?;
    let declaration = if brick.contains('/') || brick.ends_with(".toml") {
        Path::new(brick).to_path_buf()
    } else {
        native_declaration(brick)?.ok_or_else(|| {
            CliError::Usage(format!(
                "no native brick {brick:?}: give the path of a declaration file instead"
            ))
        })?
    };
    // An invalid declaration is refused when linked, with its reason.
    let loaded = load_manifest(&declaration).ok();
    if let (Some(service), Some(loaded)) = (service, &loaded) {
        check_program(brick, loaded, service)?;
    }
    let mut unconfigured = None;
    if let Some(loaded) = &loaded
        && let Some(command) = &loaded.configure
    {
        let name = loaded.name.as_str();
        if !io::stdin().is_terminal() {
            unconfigured = Some(name);
        } else if let Err(e) = configure_with(command, &loaded.dir) {
            if !force {
                return Err(CliError::Configure(format!(
                    "cannot enable {brick}: {e}. `nowcast bricks enable --force {brick}` \
                     enables it unconfigured"
                )));
            }
            eprintln!("nowcast: {e}; {name} enabled all the same (--force)");
        }
    }
    nowcast_discovery::enable(&bricks, &declaration)
        .map_err(|e| CliError::Usage(format!("cannot enable {brick}: {e}")))?;
    if let Some(name) = unconfigured {
        eprintln!(
            "nowcast: {name} enabled without its configuration, no terminal to run it in: \
             run `nowcast bricks configure {name}` in a terminal"
        );
    }
    Ok(())
}

/// Runs again the configuration command of the brick named `name`: the
/// declaration it is enabled with, otherwise the native brick's own.
pub fn configure(name: &str) -> Result<(), CliError> {
    let brick = BrickName::new(name)
        .map_err(|e| CliError::Usage(format!("invalid brick name {name:?}: {e}")))?;
    let declaration = if is_enabled(brick.as_str())? {
        link(brick.as_str())?
    } else {
        native_declaration(name)?.ok_or_else(|| {
            CliError::Usage(format!("no brick {name:?} enabled, and no native one"))
        })?
    };
    let loaded = load_manifest(&declaration).map_err(|e| {
        CliError::Usage(format!(
            "cannot read the declaration of {name} ({}): {e}",
            declaration.display()
        ))
    })?;
    let command = loaded.configure.as_ref().ok_or_else(|| {
        CliError::Usage(format!(
            "{name} has nothing to configure: its declaration has no [configure]"
        ))
    })?;
    configure_with(command, &loaded.dir).map_err(CliError::Configure)
}

/// The declaration of the native brick `name`, its embedded files written
/// first; `None` for no native brick of that name.
fn native_declaration(name: &str) -> Result<Option<PathBuf>, CliError> {
    let Some(native) = native::find(name) else {
        return Ok(None);
    };
    let dir = native_dir_from_env().map_err(|e| CliError::Usage(e.to_string()))?;
    write_native(&dir, &native.files)
        .map(Some)
        .map_err(CliError::Io)
}

/// Runs a configuration `command` in the declaration's directory, on this
/// terminal: a bare program name is looked up in the user's `PATH`, except
/// `nowcast`, this executable. Why it failed, otherwise.
fn configure_with(command: &[String], dir: &Path) -> Result<(), String> {
    let (program, args) = command
        .split_first()
        .ok_or("the configuration command is empty")?;
    let executable = if program == SELF {
        std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?
    } else {
        PathBuf::from(program)
    };
    let status = Command::new(&executable)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("cannot run the configuration {program:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the configuration {program:?} failed ({status})"))
    }
}

/// Refuses a declaration whose program is a bare name missing from the `PATH`
/// it will be looked up in: the one the declaration sets, otherwise the
/// service's. When the service cannot tell its `PATH`, a note says the
/// program went unchecked.
fn check_program(
    brick: &str,
    loaded: &BrickDeclaration,
    service: Result<Client, CliError>,
) -> Result<(), CliError> {
    let Some(program) = loaded.command.first() else {
        return Ok(());
    };
    if program.contains('/') || program == SELF {
        return Ok(());
    }
    let (path, whose) = match loaded.env.get("PATH") {
        Some(path) => (path.clone(), "the PATH its declaration sets"),
        None => match service.and_then(|client| service_path(&client)) {
            Ok(Some(path)) => (path, "the service's PATH"),
            Ok(None) => {
                eprintln!("nowcast: {program:?} not looked for: the service has no PATH");
                return Ok(());
            }
            Err(e) => {
                let reason = e.message();
                eprintln!("nowcast: {program:?} not looked for in the service's PATH: {reason}");
                return Ok(());
            }
        },
    };
    let refuse = |what: String| {
        Err(CliError::Usage(format!(
            "cannot enable {brick}: {what}. Extend that PATH (see the README, \
             \"The bricks' PATH\"), or pass --force to enable it anyway"
        )))
    };
    match find_program(program, &path, &loaded.dir) {
        Lookup::Found => Ok(()),
        Lookup::Missing => refuse(format!("program {program:?} not found in {whose} ({path})")),
        Lookup::NotExecutable(file) => refuse(format!(
            "program {program:?} found in {whose} at {}, which is not executable",
            file.display()
        )),
    }
}

fn service_path(client: &Client) -> Result<Option<String>, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::Io)?
        .block_on(client.service_path())
}

#[derive(Debug, PartialEq, Eq)]
enum Lookup {
    Found,
    Missing,
    /// No candidate is executable; the first one found.
    NotExecutable(PathBuf),
}

/// Looks `program` up in `path` as the launch will: in order, a relative
/// entry (the empty one included) standing for a directory under `dir`, the
/// brick's working directory.
fn find_program(program: &str, path: &str, dir: &Path) -> Lookup {
    let mut first = None;
    for entry in std::env::split_paths(path) {
        let candidate = dir.join(entry).join(program);
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                return Lookup::Found;
            }
            Ok(meta) if meta.is_file() => {
                first.get_or_insert(candidate);
            }
            _ => {}
        }
    }
    first.map_or(Lookup::Missing, Lookup::NotExecutable)
}

/// Disables the brick named `brick`. A brick not enabled is no error.
pub fn disable(brick: &str) -> Result<(), CliError> {
    let name = BrickName::new(brick)
        .map_err(|e| CliError::Usage(format!("invalid brick name {brick:?}: {e}")))?;
    nowcast_discovery::disable(&bricks_dir()?, &name)
        .map(drop)
        .map_err(|e| CliError::Usage(format!("cannot disable {brick}: {e}")))
}

/// Whether the brick named `name` is enabled: its link is in the bricks
/// directory.
pub fn is_enabled(name: &str) -> Result<bool, CliError> {
    Ok(std::fs::symlink_metadata(link(name)?).is_ok_and(|meta| meta.file_type().is_symlink()))
}

/// The link that enables the brick named `name`.
fn link(name: &str) -> Result<PathBuf, CliError> {
    Ok(bricks_dir()?.join(format!("{name}.toml")))
}

fn bricks_dir() -> Result<std::path::PathBuf, CliError> {
    DiscoveryConfig::from_env()
        .map(|config| config.bricks)
        .map_err(|e| CliError::Usage(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &Path, mode: u32) {
        std::fs::write(path, "").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_program_is_looked_up_in_order_and_must_be_executable() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        file(&a.join("tool"), 0o644);
        let path = std::env::join_paths([&a, &b]).unwrap();
        let path = path.to_str().unwrap();
        let dir = root.path();
        assert_eq!(
            find_program("tool", path, dir),
            Lookup::NotExecutable(a.join("tool"))
        );
        file(&b.join("tool"), 0o755);
        assert_eq!(find_program("tool", path, dir), Lookup::Found);
        assert_eq!(find_program("other", path, dir), Lookup::Missing);
    }

    #[test]
    fn an_empty_entry_stands_for_the_working_directory() {
        let root = tempfile::tempdir().unwrap();
        file(&root.path().join("tool"), 0o755);
        assert_eq!(
            find_program("tool", "/nonexistent:", root.path()),
            Lookup::Found
        );
        assert_eq!(
            find_program("tool", "/nonexistent", root.path()),
            Lookup::Missing
        );
    }

    #[test]
    fn a_relative_entry_is_under_the_working_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        file(&root.path().join("bin/tool"), 0o755);
        assert_eq!(find_program("tool", "bin", root.path()), Lookup::Found);
    }
}
