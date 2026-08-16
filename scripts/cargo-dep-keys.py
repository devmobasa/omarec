#!/usr/bin/env python3
"""Print direct Cargo dependency keys after parsing a manifest as TOML."""
from __future__ import annotations

import pathlib
import sys
import tomllib
from collections.abc import Mapping


DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} CARGO_TOML", file=sys.stderr)
        return 2

    path = pathlib.Path(sys.argv[1])
    try:
        with path.open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 2

    for table_name in DEPENDENCY_TABLES:
        dependencies = manifest.get(table_name)
        if dependencies is None:
            continue
        if not isinstance(dependencies, Mapping):
            print(f"{table_name} must be a table", file=sys.stderr)
            return 2
        for name in dependencies:
            print(name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
