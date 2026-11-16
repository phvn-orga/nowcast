//! Pressure stall information, from `/proc/pressure/{cpu,memory,io}`: the
//! share of the last 10 s during which tasks waited on a resource.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

/// `some` and `full` of a pressure file, `avg10` rounded to whole percent.
pub fn parse(text: &str) -> Map<String, Value> {
    let mut figures = Map::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(kind @ ("some" | "full")) = fields.next() else {
            continue;
        };
        if let Some(avg10) = fields
            .find_map(|f| f.strip_prefix("avg10="))
            .and_then(|v| v.parse::<f64>().ok())
        {
            figures.insert(kind.into(), json!(avg10.round() as i64));
        }
    }
    figures
}

/// The pressure of the CPU, memory and I/O under `root`; `None` without
/// `/proc/pressure` (a kernel without PSI). The CPU has no `full`: the kernel
/// keeps it at zero for the whole system.
pub fn read(root: &Path) -> Option<Value> {
    let dir = root.join("proc/pressure");
    let mut pressure = Map::new();
    for resource in ["cpu", "memory", "io"] {
        let Ok(text) = fs::read_to_string(dir.join(resource)) else {
            continue;
        };
        let mut figures = parse(&text);
        if resource == "cpu" {
            figures.remove("full");
        }
        pressure.insert(resource.into(), Value::Object(figures));
    }
    (!pressure.is_empty()).then_some(Value::Object(pressure))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Tree;

    #[test]
    fn avg10_is_rounded_and_cpu_has_no_full() {
        let tree = Tree::new();
        tree.file(
            "proc/pressure/cpu",
            "some avg10=2.50 avg60=0.00 avg300=0.00 total=83969953\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
        );
        tree.file(
            "proc/pressure/io",
            "some avg10=0.49 avg60=0.10 avg300=0.00 total=1436520\n\
             full avg10=12.70 avg60=0.00 avg300=0.00 total=1301961\n",
        );
        assert_eq!(
            read(tree.root()),
            Some(json!({ "cpu": { "some": 3 }, "io": { "some": 0, "full": 13 } }))
        );
    }

    #[test]
    fn no_pressure_files_give_nothing() {
        let tree = Tree::new();
        assert_eq!(read(tree.root()), None);
    }
}
