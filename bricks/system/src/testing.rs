//! A fake `/proc` and `/sys` in a temporary directory, for the readers'
//! tests.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

pub struct Tree {
    dir: tempfile::TempDir,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Creates the directory `rel` and returns its path.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let path = self.root().join(rel);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Writes `content` to `rel`, creating its directories.
    pub fn file(&self, rel: &str, content: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// A symbolic link at `rel` to `target`.
    pub fn link(&self, rel: &str, target: &Path) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(target, path).unwrap();
    }

    /// A hwmon sensor `hwmon` of driver `name`, tied to `device`, with
    /// inputs `(n, label, millidegrees)`.
    pub fn sensor(
        &self,
        hwmon: &str,
        name: &str,
        device: Option<&Path>,
        inputs: &[(u32, Option<&str>, &str)],
    ) {
        let dir = format!("sys/class/hwmon/{hwmon}");
        self.file(&format!("{dir}/name"), &format!("{name}\n"));
        if let Some(device) = device {
            self.link(&format!("{dir}/device"), device);
        }
        for (n, label, value) in inputs {
            self.file(&format!("{dir}/temp{n}_input"), &format!("{value}\n"));
            if let Some(label) = label {
                self.file(&format!("{dir}/temp{n}_label"), &format!("{label}\n"));
            }
        }
    }
}
