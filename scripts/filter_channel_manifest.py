#!/usr/bin/env python3
"""Filter native_artifacts entries by channel in a release manifest."""

import json
import sys
from pathlib import Path


def _load_json_tolerant(text: str):
    """Load JSON, tolerating accidental concatenated JSON documents."""
    decoder = json.JSONDecoder()
    idx = 0
    objs = []
    n = len(text)
    while idx < n:
        while idx < n and text[idx].isspace():
            idx += 1
        if idx >= n:
            break
        obj, end = decoder.raw_decode(text, idx)
        objs.append(obj)
        idx = end
    if not objs:
        raise ValueError("no JSON object found")
    if len(objs) > 1:
        print(
            f"warning: found {len(objs)} concatenated JSON objects, using last",
            file=sys.stderr,
        )
    return objs[-1]


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

    data = _load_json_tolerant(src.read_text(encoding="utf-8"))
    entries = data.get("native_artifacts", [])
    if not isinstance(entries, list):
        entries = []
    data["native_artifacts"] = [
        e for e in entries if isinstance(e, dict) and e.get("channel") == channel
    ]
    data["version"] = version
    dst.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
