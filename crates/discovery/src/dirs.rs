//! Where discovery reads its declarations.

use std::path::{Path, PathBuf};

use crate::EMBEDDED_CLIENTS;

/// The bricks directory: `$XDG_CONFIG_HOME/nowcast/bricks/`, by default
/// `~/.config/nowcast/bricks/`.
pub fn bricks_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    config_home(xdg_config_home, home).join("nowcast/bricks")
}

/// The user's client kinds directory: `$XDG_CONFIG_HOME/nowcast/clients/`, by
/// default `~/.config/nowcast/clients/`.
pub fn clients_dir(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    config_home(xdg_config_home, home).join("nowcast/clients")
}

/// Where native bricks are written: `$XDG_DATA_HOME/nowcast/bricks/`, by
/// default `~/.local/share/nowcast/bricks/`.
pub fn native_dir(xdg_data_home: Option<&Path>, home: &Path) -> PathBuf {
    base_dir(xdg_data_home, home, ".local/share").join("nowcast/bricks")
}

/// `$XDG_CONFIG_HOME`, or `~/.config` when it is unset, empty or relative.
fn config_home(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    base_dir(xdg_config_home, home, ".config")
}

/// An XDG base directory, or `home` joined with its default when it is
/// unset, empty or relative, as the XDG Base Directory specification asks.
fn base_dir(xdg: Option<&Path>, home: &Path, default: &str) -> PathBuf {
    match xdg {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(default),
    }
}

/// The variable `var` when it holds an absolute path, and `$HOME` otherwise.
/// The home directory is empty when the variable is used.
fn from_env(var: &str) -> Result<(Option<PathBuf>, PathBuf), NoHome> {
    let xdg = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let home = match (&xdg, std::env::var_os("HOME")) {
        (Some(_), _) => PathBuf::new(),
        (None, Some(home)) if !home.is_empty() => PathBuf::from(home),
        (None, _) => return Err(NoHome(var.to_owned())),
    };
    Ok((xdg, home))
}

/// `$XDG_CONFIG_HOME`, or `~/.config`, from the environment.
pub fn config_home_from_env() -> Result<PathBuf, NoHome> {
    let (xdg, home) = from_env("XDG_CONFIG_HOME")?;
    Ok(config_home(xdg.as_deref(), &home))
}

/// [`native_dir`] from `$XDG_DATA_HOME` and `$HOME`.
pub fn native_dir_from_env() -> Result<PathBuf, NoHome> {
    let (xdg, home) = from_env("XDG_DATA_HOME")?;
    Ok(native_dir(xdg.as_deref(), &home))
}

/// What a discovery reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// Holds links to brick declaration files.
    pub bricks: PathBuf,
    /// Holds the user's client kind declarations.
    pub clients: PathBuf,
    /// The client kind declarations compiled in, as (file name, contents).
    pub embedded_clients: Vec<(String, String)>,
}

/// `$HOME` is needed to find a directory and is not set: the variable named
/// is not an absolute path either.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HOME is not set and {0} is not an absolute path")]
pub struct NoHome(pub String);

impl DiscoveryConfig {
    /// The service's configuration: the directories from `$XDG_CONFIG_HOME`
    /// and `$HOME`, and [`EMBEDDED_CLIENTS`].
    pub fn from_env() -> Result<Self, NoHome> {
        let (xdg, home) = from_env("XDG_CONFIG_HOME")?;
        let xdg = xdg.as_deref();
        Ok(Self {
            bricks: bricks_dir(xdg, &home),
            clients: clients_dir(xdg, &home),
            embedded_clients: EMBEDDED_CLIENTS
                .iter()
                .map(|&(file, text)| (file.to_owned(), text.to_owned()))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        let home = Path::new("/home/user");
        assert_eq!(
            bricks_dir(Some(Path::new("relative")), home),
            Path::new("/home/user/.config/nowcast/bricks")
        );
        assert_eq!(
            clients_dir(Some(Path::new("")), home),
            Path::new("/home/user/.config/nowcast/clients")
        );
    }

    #[test]
    fn the_native_dir_follows_xdg() {
        let home = Path::new("/home/u");
        assert_eq!(
            native_dir(None, home),
            Path::new("/home/u/.local/share/nowcast/bricks")
        );
        assert_eq!(
            native_dir(Some(Path::new("relative")), home),
            Path::new("/home/u/.local/share/nowcast/bricks")
        );
        assert_eq!(
            native_dir(Some(Path::new("/data")), home),
            Path::new("/data/nowcast/bricks")
        );
    }
}
