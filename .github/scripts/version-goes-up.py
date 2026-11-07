"""Exits 0 when version $2 comes after version $1 in semver order, 1 otherwise.

Pre-releases come before their release (0.1.0-alpha.2 < 0.1.0), and their
identifiers compare as semver says: numbers numerically, below any text.
"""

import re
import sys

VERSION = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$")


def key(version: str):
    match = VERSION.match(version)
    if not match:
        sys.exit(f"{version!r} is not a version")
    major, minor, patch, pre = match.groups()
    core = (int(major), int(minor), int(patch))
    if pre is None:
        # A release sorts after every pre-release of the same core.
        return core, (1,)
    parts = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    return core, (0, parts)


sys.exit(0 if key(sys.argv[2]) > key(sys.argv[1]) else 1)
