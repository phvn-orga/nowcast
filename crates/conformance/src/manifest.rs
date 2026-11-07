//! Contract section 1, the brick declaration file, checked on whatever loads
//! one file. The fixtures are in `fixtures/manifests/`: `forge.toml` is the
//! contract's example, each `invalid-*.toml` breaks one rule of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nowcast_traits::BrickDeclaration;
use nowcast_types::BrickName;

/// Loads one brick declaration file.
pub trait ManifestSubject {
    fn load(path: &Path) -> Result<BrickDeclaration, String>;
}

/// `fixtures/manifests/`.
pub fn dir() -> PathBuf {
    crate::fixtures().join("manifests")
}

fn load<S: ManifestSubject>(name: &str) -> Result<BrickDeclaration, String> {
    S::load(&dir().join(name))
}

fn schema(name: &str) -> serde_json::Value {
    let text = std::fs::read(dir().join("schemas").join(name)).unwrap();
    serde_json::from_slice(&text).unwrap()
}

fn refused<S: ManifestSubject>(name: &str) {
    if let Ok(declaration) = load::<S>(name) {
        panic!("{name} must be refused, loaded {declaration:?}");
    }
}

/// Every field of the contract's example is read.
pub fn reads_every_field<S: ManifestSubject>() {
    let forge = load::<S>("forge.toml").expect("the contract's example is valid");
    assert_eq!(forge.name, BrickName::new("forge").unwrap());
    assert_eq!(forge.version, "0.1.0");
    assert_eq!(
        forge.description,
        "State of a PR and its CI on GitHub and Azure DevOps."
    );
    assert_eq!(forge.protocol, 1);
    assert_eq!(
        forge.env,
        BTreeMap::from([("RUST_LOG".into(), "info".into())])
    );
    let pr = &forge.subscriptions["pr"];
    assert_eq!(pr.description, "The PR of a branch and its CI checks.");
    assert_eq!(pr.publishes.as_deref(), Some("forge.pr/v1"));
    assert_eq!(pr.accepts, None);
    assert_eq!(forge.subscriptions.len(), 1);
    assert_eq!(forge.types.len(), 1);
}

/// The schemas are loaded from their paths, relative to the file.
pub fn loads_the_schemas<S: ManifestSubject>() {
    let forge = load::<S>("forge.toml").unwrap();
    assert_eq!(
        forge.subscriptions["pr"].schema,
        schema("subscription-pr.json")
    );
    assert_eq!(
        forge.types["forge.pr/v1"].schema,
        schema("forge-pr-v1.json")
    );
}

/// A relative command resolves from the file's directory; its arguments stay.
pub fn resolves_a_relative_command<S: ManifestSubject>() {
    let forge = load::<S>("forge.toml").unwrap();
    let expected = dir().join("bin/nowcast-forge");
    assert_eq!(
        forge.command,
        vec![expected.to_str().unwrap().to_owned(), "--verbose".into()]
    );
}

/// Relative paths resolve from the file's real directory, link resolved.
pub fn resolves_through_a_link<S: ManifestSubject>() {
    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("forge.toml");
    std::os::unix::fs::symlink(dir().join("forge.toml"), &link).unwrap();
    let forge = S::load(&link).expect("a linked file loads");
    assert_eq!(
        Path::new(&forge.command[0]),
        dir().join("bin/nowcast-forge")
    );
    assert_eq!(
        forge.subscriptions["pr"].schema,
        schema("subscription-pr.json")
    );
}

/// An absolute command is taken as is.
pub fn keeps_an_absolute_command<S: ManifestSubject>() {
    let absolute = load::<S>("absolute.toml").unwrap();
    assert_eq!(absolute.command, vec!["/opt/forge/bin/nowcast-forge"]);
}

/// A program name without `/` is kept, to be looked up in `PATH`.
pub fn keeps_a_bare_program_name<S: ManifestSubject>() {
    let bare = load::<S>("bare.toml").expect("bare.toml is valid");
    assert_eq!(bare.command, vec!["python3", "brick.py"]);
}

/// The brick runs in the file's real directory, link resolved.
pub fn runs_in_the_file_directory<S: ManifestSubject>() {
    let forge = load::<S>("forge.toml").unwrap();
    assert_eq!(forge.dir, dir());
    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("forge.toml");
    std::os::unix::fs::symlink(dir().join("forge.toml"), &link).unwrap();
    assert_eq!(S::load(&link).unwrap().dir, dir());
}

/// `[run] env` is optional; `version` takes any semver, pre-releases included.
pub fn accepts_the_least_a_brick_declares<S: ManifestSubject>() {
    let minimal = load::<S>("minimal.toml").expect("minimal.toml is valid");
    assert!(minimal.env.is_empty());
    assert_eq!(minimal.version, "1.2.3-beta.1");
}

/// `manifest_version` is required and is `1`.
pub fn refuses_another_manifest_version<S: ManifestSubject>() {
    refused::<S>("invalid-manifest-version.toml");
    refused::<S>("invalid-no-manifest-version.toml");
}

/// `name` is `[a-z0-9-]+`.
pub fn refuses_an_invalid_name<S: ManifestSubject>() {
    refused::<S>("invalid-name.toml");
}

/// `version` is semver.
pub fn refuses_a_non_semver_version<S: ManifestSubject>() {
    refused::<S>("invalid-version.toml");
}

/// `description`, `protocol` and `[run] command` are required.
pub fn refuses_a_missing_required_field<S: ManifestSubject>() {
    refused::<S>("invalid-no-description.toml");
    refused::<S>("invalid-no-protocol.toml");
    refused::<S>("invalid-no-command.toml");
}

/// Every `publishes` names a type declared in `[types]`.
pub fn refuses_an_undeclared_published_type<S: ManifestSubject>() {
    refused::<S>("invalid-undeclared-type.toml");
}

/// A kind may declare the type it accepts as input, with or without a
/// published type; the accepted type is declared in `[types]` like the others.
pub fn reads_an_accepted_type<S: ManifestSubject>() {
    let inbox = load::<S>("inbox.toml").unwrap();
    let both = &inbox.subscriptions["both"];
    assert_eq!(both.publishes.as_deref(), Some("forge.pr/v1"));
    assert_eq!(both.accepts.as_deref(), Some("inbox.input/v1"));
    let only = &inbox.subscriptions["only"];
    assert_eq!(only.publishes, None);
    assert_eq!(only.accepts.as_deref(), Some("inbox.input/v1"));
    assert_eq!(
        inbox.types["inbox.input/v1"].schema,
        schema("forge-pr-v1.json")
    );
}

/// Every `accepts` names a type declared in `[types]`.
pub fn refuses_an_undeclared_accepted_type<S: ManifestSubject>() {
    refused::<S>("invalid-undeclared-input.toml");
}

/// A kind publishes a type, accepts one, or both.
pub fn refuses_a_kind_without_type<S: ManifestSubject>() {
    refused::<S>("invalid-no-type.toml");
}

/// A schema path must lead to a readable JSON file, or the file is refused at
/// load.
pub fn refuses_a_missing_schema<S: ManifestSubject>() {
    refused::<S>("invalid-missing-subscription-schema.toml");
    refused::<S>("invalid-missing-type-schema.toml");
}

/// Runs the rules of contract section 1 against a declaration loader.
#[macro_export]
macro_rules! manifest_suite {
    ($subject:ty) => {
        mod manifest_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                sync,
                manifest,
                $subject,
                [
                    reads_every_field,
                    loads_the_schemas,
                    resolves_a_relative_command,
                    resolves_through_a_link,
                    keeps_an_absolute_command,
                    keeps_a_bare_program_name,
                    runs_in_the_file_directory,
                    accepts_the_least_a_brick_declares,
                    refuses_another_manifest_version,
                    refuses_an_invalid_name,
                    refuses_a_non_semver_version,
                    refuses_a_missing_required_field,
                    refuses_an_undeclared_published_type,
                    reads_an_accepted_type,
                    refuses_an_undeclared_accepted_type,
                    refuses_a_kind_without_type,
                    refuses_a_missing_schema,
                ]
            );
        }
    };
}
