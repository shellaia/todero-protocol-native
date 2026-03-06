#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: select_rollback_version.py <manifest_path> <current_version>", file=sys.stderr)
        return 2

    manifest = Path(sys.argv[1])
    current = sys.argv[2]
    data = json.loads(manifest.read_text(encoding="utf-8"))
    releases = data.get("releases", [])
    if not isinstance(releases, list) or not releases:
        print(current)
        return 0

    for release in releases:
        if isinstance(release, dict) and release.get("version") and release.get("version") != current:
            print(release["version"])
            return 0

    print(current)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
