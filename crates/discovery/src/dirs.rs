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

/// `$XDG_CONFIG_HOME`, or `~/.config` when it is unset, empty or relative, as
/// the XDG Base Directory specification asks.
fn config_home(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    match xdg_config_home {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(".config"),
    }
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

/// `$HOME` is needed to find the configuration directory and is not set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HOME is not set and XDG_CONFIG_HOME is not an absolute path")]
pub struct NoHome;

impl DiscoveryConfig {
    /// The service's configuration: the directories from `$XDG_CONFIG_HOME`
    /// and `$HOME`, and [`EMBEDDED_CLIENTS`].
    pub fn from_env() -> Result<Self, NoHome> {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let xdg = xdg.as_deref().filter(|dir| dir.is_absolute());
        let home = match (xdg, std::env::var_os("HOME")) {
            // The home directory is not used when XDG_CONFIG_HOME is valid.
            (Some(_), _) => PathBuf::new(),
            (None, Some(home)) if !home.is_empty() => PathBuf::from(home),
            (None, _) => return Err(NoHome),
        };
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
}
