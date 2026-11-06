//! Client kind declarations (contract section 5).

use std::collections::BTreeMap;
use std::path::Path;

use nowcast_traits::ClientKind;
use nowcast_types::{ClientKindName, NameError};
use serde::Deserialize;

/// Why a client kind declaration is refused.
#[derive(Debug, thiserror::Error)]
pub enum ClientKindError {
    #[error("the file name does not end in .toml")]
    NotToml,
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid name {name:?}: {source}")]
    Name { name: String, source: NameError },
    #[error("declares {name:?}, but the file is named {file:?}")]
    NameMismatch { name: String, file: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClientKind {
    name: String,
    description: String,
    anchor: RawAnchor,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnchor {
    exe: Vec<String>,
}

/// Parses the declaration held in the file `file_name`, `<name>.toml`.
///
/// An empty `exe` list is accepted: it matches no process, which lets a user
/// file switch an embedded kind off.
pub fn parse_client_kind(file_name: &str, text: &str) -> Result<ClientKind, ClientKindError> {
    let stem = file_name
        .strip_suffix(".toml")
        .ok_or(ClientKindError::NotToml)?;
    let raw: RawClientKind = toml::from_str(text)?;
    let name = ClientKindName::new(raw.name.as_str()).map_err(|source| ClientKindError::Name {
        name: raw.name.clone(),
        source,
    })?;
    if raw.name != stem {
        return Err(ClientKindError::NameMismatch {
            name: raw.name,
            file: file_name.to_owned(),
        });
    }
    Ok(ClientKind {
        name,
        description: raw.description,
        exe: raw.anchor.exe,
    })
}

/// The embedded declarations, replaced by the user's of the same name. An
/// invalid declaration is left out and logged; so are both declarations of a
/// name embedded twice.
pub(crate) fn load_client_kinds(embedded: &[(String, String)], user_dir: &Path) -> Vec<ClientKind> {
    let mut embedded_kinds = BTreeMap::new();
    let mut duplicated = Vec::new();
    for (file, text) in embedded {
        match parse_client_kind(file, text) {
            Ok(kind) => {
                if embedded_kinds.insert(kind.name.clone(), kind).is_some() {
                    duplicated.push(file);
                }
            }
            Err(e) => tracing::warn!(file, "embedded client kind refused: {e}"),
        }
    }
    for file in duplicated {
        let name = file.trim_end_matches(".toml");
        if let Ok(name) = ClientKindName::new(name)
            && embedded_kinds.remove(&name).is_some()
        {
            tracing::warn!(file, "client kind embedded twice, both refused");
        }
    }

    let mut user_kinds = read_user_dir(user_dir);
    let mut kinds: Vec<_> = embedded_kinds
        .into_values()
        .map(|kind| user_kinds.remove(&kind.name).unwrap_or(kind))
        .collect();
    kinds.extend(user_kinds.into_values());
    kinds
}

/// The valid declarations of the user directory, by name.
fn read_user_dir(dir: &Path) -> BTreeMap<ClientKindName, ClientKind> {
    let mut kinds = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "cannot read the client kinds directory: {e}");
            return kinds;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
            tracing::warn!(path = %path.display(), "client kind file name is not UTF-8, ignored");
            continue;
        };
        if !file.ends_with(".toml") || !path.is_file() {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(path = %path.display(), "cannot read the client kind: {e}");
                continue;
            }
        };
        match parse_client_kind(&file, &text) {
            Ok(kind) => {
                kinds.insert(kind.name.clone(), kind);
            }
            Err(e) => tracing::warn!(path = %path.display(), "client kind refused: {e}"),
        }
    }
    kinds
}
