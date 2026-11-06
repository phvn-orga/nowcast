//! Loading one brick declaration file (contract section 1).

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use nowcast_traits::{BrickDeclaration, SubscriptionKind, TypeDeclaration};
use nowcast_types::{BrickName, NameError};
use serde::Deserialize;

/// The only `manifest_version` this loader reads.
pub const MANIFEST_VERSION: i64 = 1;

/// Why a brick declaration file is refused.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot resolve {path}: {source}")]
    Resolve { path: PathBuf, source: io::Error },
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("manifest_version is required")]
    NoManifestVersion,
    #[error("unsupported manifest_version {0}, only {MANIFEST_VERSION} is supported")]
    UnsupportedManifestVersion(i64),
    #[error("invalid name {name:?}: {source}")]
    Name { name: String, source: NameError },
    #[error("version {version:?} is not semver: {source}")]
    Version {
        version: String,
        source: semver::Error,
    },
    #[error("[run] command is empty")]
    EmptyCommand,
    #[error("the command resolves to {path}, which is not valid UTF-8")]
    NonUtf8Command { path: PathBuf },
    #[error("subscription kind {kind:?} publishes {publishes:?}, which [types] does not declare")]
    UndeclaredType { kind: String, publishes: String },
    #[error("cannot read the schema {path}: {source}")]
    SchemaRead { path: PathBuf, source: io::Error },
    #[error("the schema {path} is not valid JSON: {source}")]
    SchemaJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("the schema {path} is not a valid JSON Schema: {message}")]
    SchemaInvalid { path: PathBuf, message: String },
}

/// Reads the version alone first, so that a file of another version is
/// refused for its version, not for fields this loader does not know.
#[derive(Deserialize)]
struct Versioned {
    manifest_version: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[allow(dead_code)]
    manifest_version: i64,
    name: String,
    version: String,
    description: String,
    protocol: u32,
    run: RawRun,
    #[serde(default)]
    subscriptions: BTreeMap<String, RawSubscription>,
    #[serde(default)]
    types: BTreeMap<String, RawType>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRun {
    command: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSubscription {
    description: String,
    schema: PathBuf,
    publishes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawType {
    schema: PathBuf,
}

/// Loads and validates a brick declaration file. Links are followed: relative
/// paths resolve from the real directory of the file.
///
/// Every schema is read, parsed and compiled as a JSON Schema; one that
/// refers to another document (`$ref` to a file or a URL) does not compile,
/// since nothing is fetched.
pub fn load_manifest(path: &Path) -> Result<BrickDeclaration, ManifestError> {
    let real = path
        .canonicalize()
        .map_err(|source| ManifestError::Resolve {
            path: path.to_path_buf(),
            source,
        })?;
    // A canonical path to a file always has a parent.
    let dir = real.parent().unwrap_or(Path::new("/")).to_path_buf();
    let text = regular_file(&real)
        .and_then(|()| std::fs::read_to_string(&real))
        .map_err(|source| ManifestError::Read {
            path: real.clone(),
            source,
        })?;

    match toml::from_str::<Versioned>(&text)?.manifest_version {
        None => return Err(ManifestError::NoManifestVersion),
        Some(MANIFEST_VERSION) => {}
        Some(other) => return Err(ManifestError::UnsupportedManifestVersion(other)),
    }
    let raw: RawManifest = toml::from_str(&text)?;

    let name = BrickName::new(raw.name.as_str()).map_err(|source| ManifestError::Name {
        name: raw.name.clone(),
        source,
    })?;
    semver::Version::parse(&raw.version).map_err(|source| ManifestError::Version {
        version: raw.version.clone(),
        source,
    })?;
    let command = resolve_command(raw.run.command, &dir)?;

    for (kind, subscription) in &raw.subscriptions {
        if !raw.types.contains_key(&subscription.publishes) {
            return Err(ManifestError::UndeclaredType {
                kind: kind.clone(),
                publishes: subscription.publishes.clone(),
            });
        }
    }
    let subscriptions = raw
        .subscriptions
        .into_iter()
        .map(|(kind, raw)| {
            let schema = load_schema(&dir, &raw.schema)?;
            let subscription = SubscriptionKind {
                description: raw.description,
                schema,
                publishes: raw.publishes,
            };
            Ok((kind, subscription))
        })
        .collect::<Result<_, ManifestError>>()?;
    let types = raw
        .types
        .into_iter()
        .map(|(ty, raw)| {
            let schema = load_schema(&dir, &raw.schema)?;
            Ok((ty, TypeDeclaration { schema }))
        })
        .collect::<Result<_, ManifestError>>()?;

    Ok(BrickDeclaration {
        name,
        version: raw.version,
        description: raw.description,
        protocol: raw.protocol,
        command,
        dir,
        env: raw.run.env,
        subscriptions,
        types,
    })
}

/// A program path (one with a `/`) resolves from `dir` when relative; a bare
/// program name is kept for a `PATH` lookup at launch.
fn resolve_command(mut command: Vec<String>, dir: &Path) -> Result<Vec<String>, ManifestError> {
    let program = command.first_mut().ok_or(ManifestError::EmptyCommand)?;
    if program.is_empty() {
        return Err(ManifestError::EmptyCommand);
    }
    if program.contains('/') {
        let resolved = normalize(&dir.join(&*program));
        // The trait holds the command as strings: a directory whose path is
        // not UTF-8 cannot be represented without changing it.
        *program = resolved
            .to_str()
            .ok_or_else(|| ManifestError::NonUtf8Command {
                path: resolved.clone(),
            })?
            .to_owned();
    }
    Ok(command)
}

/// Drops `.` components, so that `./bin/x` resolves to `<dir>/bin/x`.
fn normalize(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Reads a schema file, relative to `dir`, and checks it compiles.
fn load_schema(dir: &Path, schema: &Path) -> Result<serde_json::Value, ManifestError> {
    let path = normalize(&dir.join(schema));
    let bytes = regular_file(&path)
        .and_then(|()| std::fs::read(&path))
        .map_err(|source| ManifestError::SchemaRead {
            path: path.clone(),
            source,
        })?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| ManifestError::SchemaJson {
            path: path.clone(),
            source,
        })?;
    jsonschema::validator_for(&value).map_err(|e| ManifestError::SchemaInvalid {
        path,
        message: e.to_string(),
    })?;
    Ok(value)
}

/// Fails unless `path` leads to a regular file. Reading a FIFO would block
/// until a writer comes, and a device such as `/dev/zero` never ends.
fn regular_file(path: &Path) -> io::Result<()> {
    if std::fs::metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}
