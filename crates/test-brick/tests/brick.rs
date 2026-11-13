//! The test brick, as a process, against the brick side of contract section 4.

use nowcast_conformance::brick::BrickSubject;
use nowcast_conformance::probe;
use nowcast_test_brick::{CONTROL_VAR, MODE_VAR};
use nowcast_types::BrickName;

struct TestBrick;

impl BrickSubject for TestBrick {
    fn command() -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_nowcast-test-brick"));
        command.env_remove(MODE_VAR).env_remove(CONTROL_VAR);
        command
    }

    fn name() -> BrickName {
        probe::name()
    }

    fn subscription() -> (String, Vec<u8>) {
        (probe::KIND.into(), probe::params("a"))
    }

    fn scope() -> Option<Vec<u8>> {
        Some(probe::scope(&["a"]))
    }
}

nowcast_conformance::brick_suite!(TestBrick);

/// Tests of other packages find the binary where Cargo built it for this one.
#[test]
fn binary_is_where_cargo_builds_it() {
    let built = std::path::Path::new(env!("CARGO_BIN_EXE_nowcast-test-brick"));
    let found = nowcast_test_brick::binary();
    assert_eq!(found.canonicalize().unwrap(), built.canonicalize().unwrap());
}
