//! `nowcast bricks enable|disable`: activates a brick by linking its
//! declaration into the bricks directory, or removes the link. No call to the
//! service: it follows the directory.

use std::path::Path;

use nowcast_discovery::{DiscoveryConfig, native_dir_from_env, write_native};
use nowcast_types::BrickName;

use crate::error::CliError;
use crate::native;

/// Enables `brick`: a native brick's name, or the path of any declaration
/// file (one holding a `/` or ending in `.toml`).
pub fn enable(brick: &str) -> Result<(), CliError> {
    let bricks = bricks_dir()?;
    let declaration = if brick.contains('/') || brick.ends_with(".toml") {
        Path::new(brick).to_path_buf()
    } else {
        let native = native::find(brick).ok_or_else(|| {
            CliError::Usage(format!(
                "no native brick {brick:?}: give the path of a declaration file instead"
            ))
        })?;
        let dir = native_dir_from_env().map_err(|e| CliError::Usage(e.to_string()))?;
        write_native(&dir, &native.files).map_err(CliError::Io)?
    };
    nowcast_discovery::enable(&bricks, &declaration)
        .map(drop)
        .map_err(|e| CliError::Usage(format!("cannot enable {brick}: {e}")))
}

/// Disables the brick named `brick`. A brick not enabled is no error.
pub fn disable(brick: &str) -> Result<(), CliError> {
    let name = BrickName::new(brick)
        .map_err(|e| CliError::Usage(format!("invalid brick name {brick:?}: {e}")))?;
    nowcast_discovery::disable(&bricks_dir()?, &name)
        .map(drop)
        .map_err(|e| CliError::Usage(format!("cannot disable {brick}: {e}")))
}

fn bricks_dir() -> Result<std::path::PathBuf, CliError> {
    DiscoveryConfig::from_env()
        .map(|config| config.bricks)
        .map_err(|e| CliError::Usage(e.to_string()))
}
