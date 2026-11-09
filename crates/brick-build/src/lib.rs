//! Build-time help for the bricks native to nowcast, called from each one's
//! `build.rs`.

use std::path::PathBuf;

/// Writes the declaration the binary embeds: `brick.toml` with the workspace
/// version in place of its own, so that the version is written once, in the
/// workspace's Cargo.toml. The brick includes it as
/// `concat!(env!("OUT_DIR"), "/brick.toml")`.
pub fn embed_declaration() {
    println!("cargo::rerun-if-changed=brick.toml");
    let text = std::fs::read_to_string("brick.toml").expect("brick.toml is readable");
    let version = std::env::var("CARGO_PKG_VERSION").expect("set by cargo");
    let text = with_version(&text, &version).expect("brick.toml has no version line");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by cargo"));
    std::fs::write(out.join("brick.toml"), text).expect("OUT_DIR is writable");
}

/// `declaration` with its first `version = ` line set to `version`; `None`
/// without such a line.
fn with_version(declaration: &str, version: &str) -> Option<String> {
    let mut replaced = false;
    let lines: Vec<String> = declaration
        .lines()
        .map(|line| {
            if !replaced && line.starts_with("version = ") {
                replaced = true;
                format!("version = {version:?}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    replaced.then(|| lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_version_line_takes_the_workspace_version() {
        let declaration = "name = \"b\"\nversion = \"0.0.0\"\n[x]\nversion = \"keep\"\n";
        assert_eq!(
            with_version(declaration, "1.2.3-alpha.4").as_deref(),
            Some("name = \"b\"\nversion = \"1.2.3-alpha.4\"\n[x]\nversion = \"keep\"\n")
        );
        assert_eq!(with_version("name = \"b\"\n", "1.0.0"), None);
    }
}
