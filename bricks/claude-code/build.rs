//! Writes the declaration the binary embeds: `brick.toml` with the workspace
//! version in place of its own, so that the version is written once, in the
//! workspace's Cargo.toml.

use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=brick.toml");
    let text = std::fs::read_to_string("brick.toml").expect("brick.toml is readable");
    let version = std::env::var("CARGO_PKG_VERSION").expect("set by cargo");
    let mut replaced = false;
    let lines: Vec<String> = text
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
    assert!(replaced, "brick.toml has no version line");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by cargo"));
    std::fs::write(out.join("brick.toml"), lines.join("\n") + "\n").expect("OUT_DIR is writable");
}
