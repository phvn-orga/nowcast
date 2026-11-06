//! Brick declaration rules beyond the conformance suite.

use std::path::{Path, PathBuf};

use nowcast_discovery::{ManifestError, load_manifest};
use tempfile::TempDir;

const OBJECT: &str = r#"{ "type": "object" }"#;

/// A brick repository: a declaration file and its schema files.
struct Repo {
    _tmp: TempDir,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Self { _tmp: tmp, root }
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A valid declaration with this `[run]` table and these schemas.
    fn manifest(&self, run: &str, subscription_schema: &str, type_schema: &str) -> PathBuf {
        self.write("schemas/sub.json", subscription_schema);
        self.write("schemas/type.json", type_schema);
        self.write(
            "brick.toml",
            &format!(
                r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1

[run]
{run}

[subscriptions.pr]
description = "A PR."
schema = "schemas/sub.json"
publishes = "t/v1"

[types."t/v1"]
schema = "schemas/type.json"
"#
            ),
        )
    }
}

fn with_schema(schema: &str) -> Result<nowcast_traits::BrickDeclaration, ManifestError> {
    let repo = Repo::new();
    load_manifest(&repo.manifest(r#"command = ["bin/b"]"#, schema, OBJECT))
}

#[test]
fn the_undeclared_type_is_named() {
    let repo = Repo::new();
    repo.write("schemas/s.json", OBJECT);
    let path = repo.write(
        "brick.toml",
        r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1
run = { command = ["b"] }
subscriptions.pr = { description = "A PR.", schema = "schemas/s.json", publishes = "t/v2" }
types."t/v1" = { schema = "schemas/s.json" }
"#,
    );
    let error = load_manifest(&path).unwrap_err();
    assert!(
        matches!(&error, ManifestError::UndeclaredType { kind, publishes } if kind == "pr" && publishes == "t/v2"),
        "{error}"
    );
}

#[test]
fn a_schema_that_does_not_compile_is_refused() {
    let error = with_schema(r#"{ "type": 12 }"#).unwrap_err();
    assert!(
        matches!(error, ManifestError::SchemaInvalid { .. }),
        "{error}"
    );
}

#[test]
fn a_schema_that_is_not_json_is_refused() {
    let error = with_schema("type: object").unwrap_err();
    assert!(matches!(error, ManifestError::SchemaJson { .. }), "{error}");
}

#[test]
fn a_schema_referring_to_another_document_is_refused() {
    for reference in ["https://example.com/schema.json", "other.json"] {
        let schema = format!(r#"{{ "$ref": "{reference}" }}"#);
        let error = with_schema(&schema).unwrap_err();
        assert!(
            matches!(error, ManifestError::SchemaInvalid { .. }),
            "{reference}: {error}"
        );
    }
}

#[test]
fn a_schema_of_an_earlier_draft_is_accepted() {
    let schema = r##"{ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object",
        "definitions": { "s": { "type": "string" } }, "properties": { "a": { "$ref": "#/definitions/s" } } }"##;
    let brick = with_schema(schema).expect("a draft-07 schema compiles");
    assert_eq!(brick.subscriptions["pr"].schema["type"], "object");
}

#[test]
fn an_empty_command_is_refused() {
    for run in [r#"command = []"#, r#"command = ["", "x"]"#] {
        let repo = Repo::new();
        let error = load_manifest(&repo.manifest(run, OBJECT, OBJECT)).unwrap_err();
        assert!(
            matches!(error, ManifestError::EmptyCommand),
            "{run}: {error}"
        );
    }
}

#[test]
fn a_dot_relative_command_is_normalized() {
    let repo = Repo::new();
    let brick =
        load_manifest(&repo.manifest(r#"command = ["./bin/b", "./arg"]"#, OBJECT, OBJECT)).unwrap();
    assert_eq!(
        brick.command,
        [repo.root.join("bin/b").to_str().unwrap(), "./arg"]
    );
}

#[test]
fn an_absolute_schema_path_is_taken_as_is() {
    let repo = Repo::new();
    let elsewhere = Repo::new();
    let schema = elsewhere.write("abs.json", r#"{ "type": "string" }"#);
    let path = repo.write(
        "brick.toml",
        &format!(
            r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1
run = {{ command = ["b"] }}
types."t/v1" = {{ schema = "{}" }}
"#,
            schema.display()
        ),
    );
    let brick = load_manifest(&path).unwrap();
    assert_eq!(brick.types["t/v1"].schema["type"], "string");
}

#[test]
fn subscriptions_and_types_are_optional() {
    let repo = Repo::new();
    let path = repo.write(
        "brick.toml",
        r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1
run = { command = ["b"] }
"#,
    );
    let brick = load_manifest(&path).unwrap();
    assert!(brick.subscriptions.is_empty() && brick.types.is_empty());
}

#[test]
fn an_unknown_field_is_refused() {
    let repo = Repo::new();
    let error = load_manifest(&repo.manifest(
        "command = [\"b\"]\nenvironment = { A = \"1\" }",
        OBJECT,
        OBJECT,
    ))
    .unwrap_err();
    assert!(matches!(error, ManifestError::Toml(_)), "{error}");
}

#[test]
fn another_manifest_version_is_refused_for_its_version() {
    let repo = Repo::new();
    let path = repo.write("brick.toml", "manifest_version = 2\nnew_field = true\n");
    let error = load_manifest(&path).unwrap_err();
    assert!(
        matches!(error, ManifestError::UnsupportedManifestVersion(2)),
        "{error}"
    );
}

#[test]
fn a_broken_link_is_refused() {
    let repo = Repo::new();
    let link = repo.root.join("brick.toml");
    std::os::unix::fs::symlink(repo.root.join("missing.toml"), &link).unwrap();
    let error = load_manifest(&link).unwrap_err();
    assert!(matches!(error, ManifestError::Resolve { .. }), "{error}");
    assert!(load_manifest(Path::new("/nonexistent/brick.toml")).is_err());
}

#[test]
fn a_link_to_something_other_than_a_file_is_refused() {
    let repo = Repo::new();
    let fifo = repo.root.join("fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let link = repo.root.join("brick.toml");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    // Reading a FIFO blocks until a writer comes: run it aside, with a limit.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(load_manifest(&link).map(|_| ()));
    });
    let error = result
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("loading does not block")
        .unwrap_err();
    assert!(matches!(error, ManifestError::Read { .. }), "{error}");
}

#[test]
fn a_schema_that_is_not_a_file_is_refused() {
    let repo = Repo::new();
    let path = repo.write(
        "brick.toml",
        r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1
run = { command = ["b"] }
types."t/v1" = { schema = "/dev/zero" }
"#,
    );
    let error = load_manifest(&path).unwrap_err();
    assert!(matches!(error, ManifestError::SchemaRead { .. }), "{error}");
}

#[test]
fn a_command_path_that_is_not_utf8_is_refused() {
    use std::os::unix::ffi::OsStrExt as _;

    let repo = Repo::new();
    let dir = repo.root.join(std::ffi::OsStr::from_bytes(b"repo-\xff"));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("brick.toml");
    std::fs::write(
        &path,
        r#"
manifest_version = 1
name = "brick"
version = "0.1.0"
description = "A brick."
protocol = 1
run = { command = ["bin/b"] }
"#,
    )
    .unwrap();
    // A lossy conversion would launch a program that does not exist.
    let error = load_manifest(&path).unwrap_err();
    assert!(
        matches!(error, ManifestError::NonUtf8Command { .. }),
        "{error}"
    );
}
