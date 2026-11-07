"""Prints the cargo package arguments a pull request's changes call for.

Reads the changed files on stdin, one per line, and prints `-p <crate> ...`:
each workspace crate holding a changed file, and every crate that depends on
one, dev-dependencies included. Prints `--workspace` for a change outside the
crates (the workspace's Cargo.toml or Cargo.lock, the CI), and nothing when no
crate is concerned.
"""

import json
import subprocess
import sys

# Crates whose tests run another crate's binary, a link Cargo does not record.
RUNS = {"nowcast-e2e": ["nowcast-cli", "nowcast-test-brick"]}
# Files outside the crates that call for the whole workspace.
WHOLE = ("Cargo.toml", "Cargo.lock", ".github/workflows/ci.yml")
# Files outside the crates that a crate's build embeds.
EMBEDDED = {"clients/": "nowcast-discovery"}

metadata = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
)
root = metadata["workspace_root"].rstrip("/") + "/"
crates = {
    p["name"]: p["manifest_path"][len(root) :].rsplit("/", 1)[0] + "/"
    for p in metadata["packages"]
}
dependents: dict[str, set[str]] = {name: set() for name in crates}
for package in metadata["packages"]:
    for dep in package["dependencies"]:
        if dep["name"] in crates:
            dependents[dep["name"]].add(package["name"])
for crate, runs in RUNS.items():
    for ran in runs:
        dependents[ran].add(crate)

changed: set[str] = set()
for file in filter(None, (line.strip() for line in sys.stdin)):
    if file in WHOLE:
        print("--workspace")
        sys.exit(0)
    owners = [name for name, path in crates.items() if file.startswith(path)]
    if owners:
        # The innermost crate owns the file.
        changed.add(max(owners, key=lambda name: len(crates[name])))
    for prefix, crate in EMBEDDED.items():
        if file.startswith(prefix) and file.endswith(".toml"):
            changed.add(crate)

affected = set()
todo = list(changed)
while todo:
    crate = todo.pop()
    if crate not in affected:
        affected.add(crate)
        todo.extend(dependents[crate])
print(" ".join(f"-p {name}" for name in sorted(affected)))
