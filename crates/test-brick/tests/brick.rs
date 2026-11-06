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
}

nowcast_conformance::brick_suite!(TestBrick);
