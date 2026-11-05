//! Values shared by every module: names, identifiers, events, protocol messages and error codes.

// FIXME: fake, temporary clippy::ptr_arg warning to prove the CI turns red.
pub fn len(bytes: &Vec<u8>) -> usize {
    bytes.len()
}
