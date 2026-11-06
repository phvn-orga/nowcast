//! The test brick's declaration file against contract section 1.
//!
//! The manifest suite checks a loader, not a file. So this test holds a
//! strict reading of section 1, proves it with the suite, then loads the
//! brick's own file with it: the file must load, and declare exactly the
//! conformance probe.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use nowcast_conformance::manifest::ManifestSubject;
use nowcast_conformance::probe;
use nowcast_test_brick::manifest;
use nowcast_traits::{BrickDeclaration, SubscriptionKind, TypeDeclaration};
use nowcast_types::BrickName;
use serde::Deserialize;

#[derive(Deserialize)]
struct File {
    manifest_version: u32,
    name: String,
    version: String,
    description: String,
    protocol: u32,
    run: Run,
    #[serde(default)]
    subscriptions: BTreeMap<String, Subscription>,
    #[serde(default)]
    types: BTreeMap<String, Type>,
}

#[derive(Deserialize)]
struct Run {
    command: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Subscription {
    description: String,
    schema: PathBuf,
    publishes: String,
}

#[derive(Deserialize)]
struct Type {
    schema: PathBuf,
}

/// Reads a declaration file as contract section 1 describes it.
struct Section1;

impl ManifestSubject for Section1 {
    fn load(path: &Path) -> Result<BrickDeclaration, String> {
        let real = path.canonicalize().map_err(|e| e.to_string())?;
        let dir = real.parent().ok_or("no parent directory")?.to_owned();
        let text = fs::read_to_string(&real).map_err(|e| e.to_string())?;
        let file: File = toml::from_str(&text).map_err(|e| e.to_string())?;
        if file.manifest_version != 1 {
            return Err(format!("manifest_version {}", file.manifest_version));
        }
        semver::Version::parse(&file.version).map_err(|e| e.to_string())?;
        let mut command = file.run.command;
        let program = command.first_mut().ok_or("an empty command")?;
        if program.contains('/') && Path::new(program).is_relative() {
            *program = dir
                .join(&*program)
                .into_os_string()
                .into_string()
                .map_err(|_| "a non UTF-8 path")?;
        }
        let schema = |path: &Path| -> Result<serde_json::Value, String> {
            let bytes = fs::read(dir.join(path)).map_err(|e| format!("{path:?}: {e}"))?;
            serde_json::from_slice(&bytes).map_err(|e| format!("{path:?}: {e}"))
        };
        let mut subscriptions = BTreeMap::new();
        for (kind, sub) in file.subscriptions {
            if !file.types.contains_key(&sub.publishes) {
                return Err(format!("{kind} publishes undeclared {}", sub.publishes));
            }
            let schema = schema(&sub.schema)?;
            subscriptions.insert(
                kind,
                SubscriptionKind {
                    description: sub.description,
                    schema,
                    publishes: sub.publishes,
                },
            );
        }
        let mut types = BTreeMap::new();
        for (name, ty) in file.types {
            let schema = schema(&ty.schema)?;
            types.insert(name, TypeDeclaration { schema });
        }
        Ok(BrickDeclaration {
            name: BrickName::new(file.name).map_err(|e| e.to_string())?,
            version: file.version,
            description: file.description,
            protocol: file.protocol,
            command,
            dir,
            env: file.run.env,
            subscriptions,
            types,
        })
    }
}

nowcast_conformance::manifest_suite!(Section1);

/// The shipped file loads, and declares the probe of the API and supervisor
/// suites, launched by the bare program name.
#[test]
fn the_brick_declares_the_probe() {
    let loaded = Section1::load(&manifest()).expect("the test brick's manifest is valid");
    let dir = manifest().parent().unwrap().canonicalize().unwrap();
    assert_eq!(
        loaded,
        probe::declaration(vec!["nowcast-test-brick".into()], dir)
    );
}

/// The constants of the library are the probe's.
#[test]
fn the_constants_are_the_probe_s() {
    assert_eq!(nowcast_test_brick::NAME, probe::NAME);
    assert_eq!(nowcast_test_brick::KIND, probe::KIND);
    assert_eq!(nowcast_test_brick::TYPE, probe::TYPE);
    assert_eq!(
        nowcast_test_brick::REFUSAL_CODE,
        nowcast_conformance::api::REFUSAL_CODE
    );
}
