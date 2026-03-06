#!/usr/bin/env python3
"""Filter native_artifacts entries by channel in a release manifest."""

import json
import sys
from pathlib import Path


def load_first_json_doc(text: str):
    decoder = json.JSONDecoder()
    idx = 0
    n = len(text)
    while idx < n and text[idx].isspace():
        idx += 1
    if idx >= n:
        raise ValueError("empty JSON payload")
    obj, end = decoder.raw_decode(text, idx)
    trailing = text[end:].strip()
    return obj, trailing


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

    raw = src.read_text(encoding="utf-8")
    data, trailing = load_first_json_doc(raw)
    if trailing:
        # Canonicalize file if upstream accidentally appended extra JSON/content.
        src.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
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
