//! Loading and watching brick manifests and client kind declarations.
//!
//! - [`load_manifest`] reads and validates one brick declaration file
//!   (contract section 1), schemas included.
//! - [`FsDiscovery`] implements [`nowcast_traits::Discovery`]: it reads the
//!   bricks directory, where a link `<name>.toml` activates a brick, and the
//!   client kinds directory, then follows both with inotify (sections 2
//!   and 5). Invalid files are left out and logged with `tracing`.
//! - [`write_native`], [`enable`] and [`disable`] write a native brick's
//!   embedded files and link a declaration into the bricks directory.
//! - [`EMBEDDED_CLIENTS`] holds the client kind declarations of the
//!   repository's `clients/*/` directories, compiled in.

mod client_kind;
mod dirs;
mod manifest;
mod native;
mod watch;

pub use client_kind::{ClientKindError, parse_client_kind};
pub use dirs::{DiscoveryConfig, NoHome, bricks_dir, clients_dir};
pub use manifest::{MANIFEST_VERSION, ManifestError, load_manifest};
pub use native::{
    DECLARATION, EnableError, NativeFiles, disable, enable, native_dir, native_dir_from_env,
    refresh_natives, write_native,
};
pub use watch::{FsDiscovery, StartError};

/// The client kind declarations compiled in, as (file name, contents): every
/// `clients/<adapter>/<kind>.toml` of the repository. Empty when it has none.
pub static EMBEDDED_CLIENTS: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/embedded_clients.rs"));
