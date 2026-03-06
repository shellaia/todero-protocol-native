#!/usr/bin/env python3
import json
import re
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 5:
        print(
            "usage: validate_bucket_manifest.py <manifest_path> <version> <expected_snapshot> <expected_alias>",
            file=sys.stderr,
        )
        return 2

    manifest_path = Path(sys.argv[1])
    version = sys.argv[2]
    expected_snapshot = sys.argv[3]
    expected_alias = sys.argv[4]

    data = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit("bucket manifest root must be an object")
    if not isinstance(data.get("releases"), list):
        raise SystemExit("bucket manifest releases must be a list")

    releases = data["releases"]
    if not releases:
        raise SystemExit("bucket manifest releases must not be empty")

    iso_utc = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
    versions = []
    for i, release in enumerate(releases):
        if not isinstance(release, dict):
            raise SystemExit(f"releases[{i}] must be object")
        for key in ("version", "publishedAt", "snapshot", "alias"):
            if not release.get(key):
                raise SystemExit(f"releases[{i}] missing {key}")
        if not iso_utc.match(str(release["publishedAt"])):
            raise SystemExit(f"releases[{i}] invalid publishedAt format: {release['publishedAt']}")
        versions.append(release["version"])

    if len(set(versions)) != len(versions):
        raise SystemExit("bucket manifest contains duplicate versions")

    matching = [release for release in releases if release.get("version") == version]
    if len(matching) != 1:
        raise SystemExit(f"expected exactly one entry for current version={version}, found={len(matching)}")
    current = matching[0]
    if current["snapshot"] != expected_snapshot:
        raise SystemExit(f"snapshot pointer mismatch: {current['snapshot']} != {expected_snapshot}")
    if current["alias"] != expected_alias:
        raise SystemExit(f"alias pointer mismatch: {current['alias']} != {expected_alias}")

    times = [release["publishedAt"] for release in releases]
    if times != sorted(times, reverse=True):
        raise SystemExit("bucket manifest releases are not sorted by publishedAt desc")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
