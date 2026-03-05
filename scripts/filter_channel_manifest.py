#!/usr/bin/env python3
"""Filter native_artifacts entries by channel in a release manifest."""

import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: filter_channel_manifest.py <in_manifest> <out_manifest> <channel> <version>"
        )

    in_manifest, out_manifest, channel, version = sys.argv[1:]
    src = Path(in_manifest)
    dst = Path(out_manifest)

    if not src.is_file():
        raise SystemExit(f"input manifest not found: {src}")

    data = json.loads(src.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit("manifest root must be a JSON object")

    entries = data.get("native_artifacts", [])
    if not isinstance(entries, list):
        raise SystemExit("native_artifacts must be a list")
    total_entries = len(entries)
    filtered = [
        e for e in entries if isinstance(e, dict) and e.get("channel") == channel
    ]
    if not filtered:
        raise SystemExit(
            f"filtered manifest has no entries for channel={channel} (input entries={total_entries})"
        )
    data["native_artifacts"] = filtered
    data["version"] = version
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
