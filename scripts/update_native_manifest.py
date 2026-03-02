#!/usr/bin/env python3
"""Update native release manifest JSON in-place for one published version."""

import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 7:
        raise SystemExit(
            "usage: update_native_manifest.py <src> <dst> <version> <published_at> <snapshot> <alias>"
        )

    src, dst, version, published_at, snapshot, alias = sys.argv[1:]
    src_path = Path(src)
    dst_path = Path(dst)

    try:
        data = json.loads(src_path.read_text(encoding="utf-8"))
    except Exception:
        data = {"schema": 1, "project": "todero-native", "channel": "native", "releases": []}

    if not isinstance(data, dict):
        data = {"schema": 1, "project": "todero-native", "channel": "native", "releases": []}

    data.setdefault("schema", 1)
    data.setdefault("project", "todero-native")
    data["channel"] = "native"

    releases = data.get("releases")
    if not isinstance(releases, list):
        releases = []
    releases = [r for r in releases if isinstance(r, dict) and r.get("version") != version]
    releases.append(
        {
            "version": version,
            "publishedAt": published_at,
            "snapshot": snapshot,
            "alias": alias,
        }
    )
    releases.sort(key=lambda r: r.get("publishedAt", ""), reverse=True)
    data["releases"] = releases

    dst_path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
