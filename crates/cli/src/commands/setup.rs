//! `nowcast setup`: writes the systemd user units that start the service on
//! the first connection, enables its socket, and enables the native bricks
//! chosen. `nowcast setup --remove` undoes the systemd part.
//!
//! The units are this binary's own, so that one written by an older version
//! is rewritten by the next `setup`. The service runs from the path of this
//! binary, or from Homebrew's opt path when it lives in the Cellar: the next
//! `brew upgrade` removes the versioned path.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use cliclack::log;
use nowcast_daemon::{SERVICE_UNIT, SOCKET_UNIT};
use nowcast_discovery::config_home_from_env;

use crate::client::Client;
use crate::commands::bricks;
use crate::error::CliError;
use crate::native::{self, NATIVES, Native};

const SOCKET: &str = "nowcast.socket";
const SERVICE: &str = "nowcast.service";

/// Sets the service up, then enables the native bricks named in `chosen`.
/// With none, and a terminal to ask in, asks which native bricks to enable.
pub fn setup(
    chosen: &[String],
    client: &dyn Fn() -> Result<Client, CliError>,
) -> Result<(), CliError> {
    let mut wanted = Vec::new();
    for name in chosen {
        let native = native::find(name).ok_or_else(|| {
            CliError::Usage(format!(
                "no native brick {name:?}: setup enables the native bricks only, \
                 `nowcast bricks enable` any other"
            ))
        })?;
        wanted.push((native, true));
    }
    let (units, exec, expected) = expected_units()?;
    let ask = chosen.is_empty() && io::stdin().is_terminal() && io::stderr().is_terminal();

    cliclack::intro("nowcast setup")?;
    if ask {
        // Cancelled, the prompt has said so already: nothing changed.
        match choose()? {
            Some(choice) => wanted = choice,
            None => return Ok(()),
        }
    }

    for (name, text) in &expected {
        let path = units.join(name);
        let written = write_unit(&path, text)
            .map_err(|e| CliError::Setup(format!("cannot write {}: {e}", path.display())))?;
        match written {
            Written::Created => log::success(format!("{name} written in {}", units.display()))?,
            Written::Rewritten => log::success(format!("{name} rewritten, it differed"))?,
            Written::Unchanged => log::info(format!("{name} up to date"))?,
        }
    }
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", SOCKET])?;
    log::success(format!("{SOCKET} enabled, running {exec}"))?;

    for (native, enable) in &wanted {
        let name = native.files.name;
        if *enable {
            bricks::enable(name, Some(client()))?;
            log::success(format!("Brick {name} enabled"))?;
        } else {
            bricks::disable(name)?;
            log::success(format!("Brick {name} disabled"))?;
        }
    }
    if !ask && wanted.is_empty() {
        log::info("No brick enabled: name them, or run setup in a terminal to choose")?;
    }
    cliclack::outro(
        "The service starts on the first connection: `nowcast status` tells what runs",
    )?;
    Ok(())
}

/// Asks which native bricks to enable, the enabled ones checked. Returns the
/// bricks whose state changes, and whether each one is to be enabled; `None`
/// when the question is cancelled.
fn choose() -> Result<Option<Vec<(&'static Native, bool)>>, CliError> {
    let mut enabled = Vec::new();
    for native in NATIVES {
        if bricks::is_enabled(native.files.name)? {
            enabled.push(native.files.name);
        }
    }
    let mut prompt = cliclack::multiselect("Bricks to enable (space to check, enter to confirm)")
        .required(false)
        .initial_values(enabled.clone());
    for native in NATIVES {
        prompt = prompt.item(native.files.name, native.files.name, native.description);
    }
    let checked = match prompt.interact() {
        Ok(checked) => checked,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(
        NATIVES
            .iter()
            .map(|n| (n, checked.contains(&n.files.name)))
            .filter(|(n, enable)| *enable != enabled.contains(&n.files.name))
            .collect(),
    ))
}

/// Stops the service, disables its socket and removes both units. The bricks
/// stay as they are.
pub fn remove() -> Result<(), CliError> {
    let units = units_dir()?;
    cliclack::intro("nowcast setup --remove")?;
    let present: Vec<&str> = [SOCKET, SERVICE]
        .into_iter()
        .filter(|name| fs::symlink_metadata(units.join(name)).is_ok())
        .collect();
    if present.is_empty() {
        cliclack::outro(format!(
            "No nowcast unit in {}: nothing to remove",
            units.display()
        ))?;
        return Ok(());
    }
    // A unit whose link the last upgrade broke may not stop: it is removed
    // all the same.
    let mut stop = vec!["stop"];
    stop.extend(&present);
    if let Err(e) = systemctl(&stop) {
        log::warning(e.message())?;
    }
    let wants = units.join("sockets.target.wants").join(SOCKET);
    for path in std::iter::once(wants).chain(present.iter().map(|name| units.join(name))) {
        match fs::remove_file(&path) {
            Ok(()) => log::success(format!("{} removed", path.display()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(CliError::Setup(format!(
                    "cannot remove {}: {e}",
                    path.display()
                )));
            }
        }
    }
    systemctl(&["daemon-reload"])?;
    cliclack::outro("The service is stopped and will not start again")?;
    Ok(())
}

/// Whether the units in place are the ones `setup` would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    Current,
    /// One is missing, differs or is a link: an upgrade changed them, or an
    /// install by hand linked them.
    Outdated,
    /// Neither is there: `setup` never ran.
    Missing,
}

impl Units {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Outdated => "outdated",
            Self::Missing => "missing",
        }
    }
}

/// The state of the units in place, `None` when it cannot be told: no home
/// directory, or a binary `setup` would refuse to run the service from.
pub fn units_state() -> Option<Units> {
    let (units, _, expected) = expected_units().ok()?;
    let mut present = 0;
    let mut current = 0;
    for (name, text) in &expected {
        let path = units.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                present += 1;
                let same = !meta.file_type().is_symlink()
                    && fs::read(&path).is_ok_and(|held| held == text.as_bytes());
                current += usize::from(same);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(match (present, current) {
        (0, _) => Units::Missing,
        (p, c) if p == expected.len() && c == p => Units::Current,
        _ => Units::Outdated,
    })
}

/// Each unit's file name and text.
type UnitFiles = [(&'static str, String); 2];

/// Where the units go, the path the service runs from, and the units.
fn expected_units() -> Result<(PathBuf, String, UnitFiles), CliError> {
    let units = units_dir()?;
    let exec = exec_path(&service_executable(&std::env::current_exe()?))?;
    let service = SERVICE_UNIT.replace("@NOWCAST@", &exec);
    Ok((
        units,
        exec,
        [(SOCKET, SOCKET_UNIT.to_owned()), (SERVICE, service)],
    ))
}

/// `$XDG_CONFIG_HOME/systemd/user/`, where systemd reads the user's units.
fn units_dir() -> Result<PathBuf, CliError> {
    config_home_from_env()
        .map(|config| config.join("systemd/user"))
        .map_err(|e| CliError::Usage(e.to_string()))
}

/// The path the service runs from: Homebrew's opt path for a binary in its
/// Cellar (`<prefix>/Cellar/<formula>/<version>/bin/<binary>`), the
/// binary's own path otherwise.
fn service_executable(exe: &Path) -> PathBuf {
    let parts: Vec<Component> = exe.components().collect();
    if let [prefix @ .., cellar, formula, _version, bin, binary] = parts.as_slice()
        && cellar.as_os_str() == "Cellar"
        && bin.as_os_str() == "bin"
    {
        let prefix: PathBuf = prefix.iter().collect();
        return prefix.join("opt").join(formula).join("bin").join(binary);
    }
    exe.to_path_buf()
}

/// `path` as `ExecStart` takes it. A path systemd would read otherwise (a
/// space, a quote, a `%` specifier, a `$` variable) is refused.
fn exec_path(path: &Path) -> Result<String, CliError> {
    let text = path.to_str().unwrap_or_default();
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._+-@".contains(c);
    if text.is_empty() || !text.chars().all(plain) {
        return Err(CliError::Setup(format!(
            "cannot run the service from {}: systemd needs a path made of \
             letters, digits and /._+-@",
            path.display()
        )));
    }
    Ok(text.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum Written {
    Created,
    /// The file differed, or was a link: an install by hand linked the units.
    Rewritten,
    Unchanged,
}

/// Writes `text` at `path` as a regular file, unless it already holds it.
fn write_unit(path: &Path, text: &str) -> io::Result<Written> {
    let written = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Writing through the link would change its target.
            fs::remove_file(path)?;
            Written::Rewritten
        }
        Ok(_) if fs::read(path)? == text.as_bytes() => return Ok(Written::Unchanged),
        Ok(_) => Written::Rewritten,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Written::Created,
        Err(e) => return Err(e),
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)?;
    Ok(written)
}

/// Runs `systemctl --user` with `args`, its failure carrying its message.
fn systemctl(args: &[&str]) -> Result<(), CliError> {
    let command = format!("systemctl --user {}", args.join(" "));
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| CliError::Setup(format!("cannot run {command}: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr);
    Err(CliError::Setup(format!(
        "{command} failed ({}): {}",
        output.status,
        reason.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cellar_binary_runs_from_the_opt_path() {
        let exe = Path::new("/home/linuxbrew/.linuxbrew/Cellar/nowcast/0.1.0-alpha.6/bin/nowcast");
        assert_eq!(
            service_executable(exe),
            Path::new("/home/linuxbrew/.linuxbrew/opt/nowcast/bin/nowcast")
        );
    }

    #[test]
    fn any_other_binary_runs_from_its_own_path() {
        for exe in [
            "/home/u/.local/bin/nowcast",
            "/opt/Cellar/bin/nowcast",
            "/x/Cellar/nowcast/1.0/libexec/nowcast",
        ] {
            assert_eq!(service_executable(Path::new(exe)), Path::new(exe));
        }
    }

    #[test]
    fn a_path_systemd_would_read_otherwise_is_refused() {
        let ok = "/home/linuxbrew/.linuxbrew/opt/nowcast/bin/nowcast";
        assert_eq!(exec_path(Path::new(ok)).unwrap(), ok);
        for bad in [
            "/home/a b/nowcast",
            "/x/%h/nowcast",
            "/x/$HOME/nowcast",
            "/x/\"/nowcast",
        ] {
            assert!(
                matches!(exec_path(Path::new(bad)), Err(CliError::Setup(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_service_unit_takes_the_binary_path() {
        let unit = SERVICE_UNIT.replace("@NOWCAST@", "/b/nowcast");
        assert!(
            unit.lines().any(|l| l == "ExecStart=/b/nowcast daemon"),
            "{unit}"
        );
        assert!(!unit.contains('@'), "{unit}");
    }

    #[test]
    fn a_unit_is_written_rewritten_or_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user/a.service");
        assert_eq!(write_unit(&path, "one").unwrap(), Written::Created);
        assert_eq!(write_unit(&path, "one").unwrap(), Written::Unchanged);
        assert_eq!(write_unit(&path, "two").unwrap(), Written::Rewritten);
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn a_linked_unit_becomes_a_file_and_its_target_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.service");
        fs::write(&target, "old").unwrap();
        let path = dir.path().join("a.service");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(write_unit(&path, "old").unwrap(), Written::Rewritten);
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }
}
