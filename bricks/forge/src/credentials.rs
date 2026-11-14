//! Where each forge's token comes from. The user chooses, forge by forge, at
//! the brick's configuration ([`crate::configure()`]): the forge's CLI, or a
//! token they give. The choice is kept in the brick's own configuration file,
//! which holds no secret; a token goes to the system keyring (Secret
//! Service). Only the chosen source is read, and nothing chosen gives no
//! token.

use std::future::Future;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The configuration file, under `$XDG_CONFIG_HOME`: beside the bricks
/// directory (`nowcast/bricks/`), not in it.
const CONFIG: &str = "nowcast/forge/config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Forge {
    GitHub,
    Azure,
}

impl Forge {
    pub const ALL: [Self; 2] = [Self::GitHub, Self::Azure];

    /// Its table in the configuration file, and its keyring attribute.
    pub const fn key(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::Azure => "azure",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::Azure => "Azure DevOps",
        }
    }
}

/// Where a forge's token comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The forge's CLI: `gh`, `az`.
    Cli,
    /// A token the user gave, in the keyring.
    Token,
}

/// The configuration file: a table per forge used. A forge without one is
/// not used.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github: Option<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    azure: Option<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    source: Source,
}

impl Config {
    pub fn source(&self, forge: Forge) -> Option<Source> {
        self.slot(forge).as_ref().map(|c| c.source)
    }

    pub fn set(&mut self, forge: Forge, source: Option<Source>) {
        *self.slot_mut(forge) = source.map(|source| Choice { source });
    }

    fn slot(&self, forge: Forge) -> &Option<Choice> {
        match forge {
            Forge::GitHub => &self.github,
            Forge::Azure => &self.azure,
        }
    }

    fn slot_mut(&mut self, forge: Forge) -> &mut Option<Choice> {
        match forge {
            Forge::GitHub => &mut self.github,
            Forge::Azure => &mut self.azure,
        }
    }

    /// Reads the file at `path`; an absent file is an empty configuration.
    pub fn read(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the file at `path`, whole or not at all.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        let text = format!("# Written by `nowcast bricks configure forge`.\n{text}");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let partial = path.with_extension("toml.partial");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, path)
    }
}

/// The configuration file as seen from outside: its modification time,
/// inode and size, `None` when it is absent. Each configuration rewrites the
/// file whole, a new token included, so a token obtained under one stamp may
/// come from a source no longer chosen under another.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(Option<(Option<SystemTime>, u64, u64)>);

/// A future the [`Secrets`] trait returns.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the tokens the user gives are kept.
pub trait Secrets: Send + Sync {
    /// The forge's token; `None` when none is kept or the keyring cannot
    /// be read.
    fn get(&self, forge: Forge) -> Pending<'_, Option<String>>;
    fn set<'a>(&'a self, forge: Forge, token: &'a str) -> Pending<'a, Result<(), String>>;
    fn delete(&self, forge: Forge) -> Pending<'_, Result<(), String>>;
}

/// What the providers read their credentials from.
#[derive(Clone)]
pub struct Credentials {
    /// `None` without a home directory: nothing is configured.
    config: Option<PathBuf>,
    secrets: Arc<dyn Secrets>,
}

impl Credentials {
    /// The user's configuration file and the system keyring.
    pub fn system() -> Self {
        Self::new(config_path_from_env(), Arc::new(Keyring))
    }

    pub fn new(config: Option<PathBuf>, secrets: Arc<dyn Secrets>) -> Self {
        Self { config, secrets }
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config.as_deref()
    }

    pub fn secrets(&self) -> &dyn Secrets {
        &*self.secrets
    }

    /// The source chosen for `forge`, the file read anew; `None` when none
    /// is, or the file cannot be read (logged).
    pub fn source(&self, forge: Forge) -> Option<Source> {
        match Config::read(self.config.as_deref()?) {
            Ok(config) => config.source(forge),
            Err(e) => {
                crate::log(format_args!("cannot read the configuration: {e}"));
                None
            }
        }
    }

    /// The configuration file's stamp, read anew.
    pub fn stamp(&self) -> Stamp {
        let metadata = self
            .config
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok());
        Stamp(metadata.map(|m| (m.modified().ok(), m.ino(), m.len())))
    }

    /// The token the user gave for `forge`, from the keyring.
    pub async fn token(&self, forge: Forge) -> Option<String> {
        self.secrets
            .get(forge)
            .await
            .filter(|token| !token.is_empty())
    }
}

/// `$XDG_CONFIG_HOME/nowcast/forge/config.toml`, `~/.config` standing for
/// `$XDG_CONFIG_HOME` when it is unset, empty or relative; `None` without a
/// home either.
pub fn config_path_from_env() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let base = match xdg {
        Some(dir) => dir,
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?).join(".config"),
    };
    Some(base.join(CONFIG))
}

/// The system keyring, through the Secret Service.
struct Keyring;

impl Keyring {
    fn attributes(forge: Forge) -> [(&'static str, &'static str); 3] {
        [
            ("application", "nowcast"),
            ("brick", crate::NAME),
            ("forge", forge.key()),
        ]
    }
}

impl Secrets for Keyring {
    fn get(&self, forge: Forge) -> Pending<'_, Option<String>> {
        Box::pin(async move {
            let keyring = oo7::Keyring::new().await.ok()?;
            let items = keyring.search_items(&Self::attributes(forge)).await.ok()?;
            let secret = items.first()?.secret().await.ok()?;
            secret.as_str().map(str::to_owned)
        })
    }

    fn set<'a>(&'a self, forge: Forge, token: &'a str) -> Pending<'a, Result<(), String>> {
        Box::pin(async move {
            let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
            keyring.unlock().await.map_err(|e| e.to_string())?;
            let label = format!("nowcast forge: {} token", forge.title());
            keyring
                .create_item(&label, &Self::attributes(forge), token, true)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn delete(&self, forge: Forge) -> Pending<'_, Result<(), String>> {
        Box::pin(async move {
            let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
            keyring.unlock().await.map_err(|e| e.to_string())?;
            keyring
                .delete(&Self::attributes(forge))
                .await
                .map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;

    use super::*;

    /// Tokens kept in memory, for tests.
    #[derive(Default)]
    pub struct MemorySecrets(std::sync::Mutex<HashMap<Forge, String>>);

    impl MemorySecrets {
        pub fn kept(&self, forge: Forge) -> Option<String> {
            self.0.lock().unwrap().get(&forge).cloned()
        }
    }

    impl Secrets for MemorySecrets {
        fn get(&self, forge: Forge) -> Pending<'_, Option<String>> {
            let token = self.0.lock().unwrap().get(&forge).cloned();
            Box::pin(async move { token })
        }

        fn set<'a>(&'a self, forge: Forge, token: &'a str) -> Pending<'a, Result<(), String>> {
            self.0.lock().unwrap().insert(forge, token.to_owned());
            Box::pin(async { Ok(()) })
        }

        fn delete(&self, forge: Forge) -> Pending<'_, Result<(), String>> {
            self.0.lock().unwrap().remove(&forge);
            Box::pin(async { Ok(()) })
        }
    }

    /// Credentials with nothing configured.
    pub fn none() -> Credentials {
        Credentials::new(None, Arc::new(MemorySecrets::default()))
    }

    /// A configuration file in its own directory, and tokens in memory.
    pub struct Fake {
        _dir: tempfile::TempDir,
        pub secrets: Arc<MemorySecrets>,
        pub credentials: Credentials,
    }

    impl Fake {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let secrets = Arc::new(MemorySecrets::default());
            let path = dir.path().join("forge/config.toml");
            let credentials = Credentials::new(Some(path), secrets.clone());
            Self {
                _dir: dir,
                secrets,
                credentials,
            }
        }

        /// Chooses `source` for `forge`, and keeps `token` for it.
        pub fn choose(&self, forge: Forge, source: Option<Source>, token: Option<&str>) {
            let path = self.credentials.config_path().unwrap();
            let mut config = Config::read(path).unwrap();
            config.set(forge, source);
            config.write(path).unwrap();
            let mut kept = self.secrets.0.lock().unwrap();
            match token {
                Some(token) => kept.insert(forge, token.to_owned()),
                None => kept.remove(&forge),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_keeps_a_table_per_forge_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge/config.toml");
        assert_eq!(Config::read(&path).unwrap(), Config::default());
        let mut config = Config::default();
        config.set(Forge::GitHub, Some(Source::Token));
        config.set(Forge::Azure, Some(Source::Cli));
        config.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[github]\nsource = \"token\""), "{text}");
        assert!(text.contains("[azure]\nsource = \"cli\""), "{text}");
        let read = Config::read(&path).unwrap();
        assert_eq!(read, config);
        config.set(Forge::Azure, None);
        config.write(&path).unwrap();
        let read = Config::read(&path).unwrap();
        assert_eq!(read.source(Forge::Azure), None);
        assert_eq!(read.source(Forge::GitHub), Some(Source::Token));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("azure"));
    }

    #[test]
    fn an_unreadable_configuration_chooses_nothing() {
        let fake = testing::Fake::new();
        let path = fake.credentials.config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[github]\nsource = \"browser\"\n").unwrap();
        assert!(Config::read(path).is_err());
        assert_eq!(fake.credentials.source(Forge::GitHub), None);
        assert_eq!(testing::none().source(Forge::GitHub), None);
    }
}
