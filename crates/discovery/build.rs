//! Embeds the client kind declarations the repository ships: every
//! `clients/<adapter>/<kind>.toml` at the workspace root, `<kind>` being a
//! valid kind name (`[a-z0-9-]+`). Other files, such as an adapter's
//! `Cargo.toml`, are not declarations and are skipped. The build works with no
//! `clients/` directory at all.
//!
//! The declarations are checked at run time, like the user's. Only a kind
//! declared twice, by two adapters, fails the build.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let clients = manifest_dir.join("../../clients");
    // Cargo reruns a build script at every build while a watched path is
    // missing, which would rebuild this crate and its dependents each time.
    // So `clients/` is watched only once it exists: whoever creates it first
    // runs `cargo clean -p nowcast-discovery` once, or touches this file.
    if clients.is_dir() {
        println!("cargo::rerun-if-changed={}", clients.display());
    } else {
        println!("cargo::rerun-if-changed=build.rs");
    }

    let declarations = find(&clients);
    let mut code = String::from("&[\n");
    for (kind, path) in &declarations {
        let path = path
            .canonicalize()
            .unwrap_or_else(|e| panic!("cannot resolve {}: {e}", path.display()));
        let path = path
            .to_str()
            .unwrap_or_else(|| panic!("{} is not valid UTF-8", path.display()));
        writeln!(
            code,
            "    ({:?}, include_str!({path:?})),",
            format!("{kind}.toml")
        )
        .unwrap();
    }
    code.push(']');

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by cargo"));
    fs::write(out.join("embedded_clients.rs"), code).expect("OUT_DIR is writable");
}

/// The declarations under `clients/`, by kind name.
fn find(clients: &Path) -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    let Ok(adapters) = fs::read_dir(clients) else {
        return found;
    };
    for adapter in adapters {
        let adapter = adapter.expect("clients/ is readable").path();
        if !adapter.is_dir() {
            continue;
        }
        let files = fs::read_dir(&adapter)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", adapter.display()));
        for file in files {
            let path = file.expect("an adapter directory is readable").path();
            let Some(kind) = kind_name(&path) else {
                continue;
            };
            if let Some(first) = found.insert(kind.clone(), path.clone()) {
                panic!(
                    "client kind {kind:?} is declared twice: {} and {}",
                    first.display(),
                    path.display()
                );
            }
        }
    }
    found
}

/// The kind a file declares, if it is named `<kind>.toml`.
fn kind_name(path: &Path) -> Option<String> {
    if !path.is_file() || path.extension()? != "toml" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let valid = !stem.is_empty()
        && stem
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'));
    valid.then(|| stem.to_owned())
}
