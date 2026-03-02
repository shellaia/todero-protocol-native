#!/usr/bin/env python3
"""Filter native_artifacts entries by channel in a release manifest."""

import json
import sys
from pathlib import Path


def _log(msg: str) -> None:
    print(f"[filter_channel_manifest] {msg}", file=sys.stderr)


def _snippet(text: str, start: int, size: int = 120) -> str:
    s = max(0, start)
    e = min(len(text), s + size)
    return text[s:e].replace("\n", "\\n")


def _load_json_tolerant(text: str):
    """Load JSON, tolerating concatenated JSON and stray non-JSON text."""
    decoder = json.JSONDecoder()
    idx = 0
    objs = []
    n = len(text)
    parse_attempts = 0
    parse_failures = 0
    _log(f"input_size_bytes={n}")
    while idx < n:
        # Skip whitespace and seek next plausible JSON start token.
        while idx < n and text[idx].isspace():
            idx += 1
        while idx < n and text[idx] not in "{[":
            idx += 1
        if idx >= n:
            break
        parse_attempts += 1
        try:
            obj, end = decoder.raw_decode(text, idx)
        except json.JSONDecodeError:
            # Move forward and try to resynchronize on next JSON token.
            parse_failures += 1
            _log(
                "decode_error"
                f" attempt={parse_attempts}"
                f" offset={idx}"
                f" context='{_snippet(text, idx)}'"
            )
            idx += 1
            continue
        _log(
            "decode_ok"
            f" attempt={parse_attempts}"
            f" start={idx}"
            f" end={end}"
            f" type={type(obj).__name__}"
        )
        objs.append(obj)
        idx = end
    if not objs:
        raise ValueError("no JSON object found")
    _log(
        "parse_summary"
        f" attempts={parse_attempts}"
        f" failures={parse_failures}"
        f" objects={len(objs)}"
    )
    if len(objs) > 1:
        _log(f"warning: found {len(objs)} JSON objects, selecting last object")
    return objs[-1]


def main() -> int:
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: filter_channel_manifest.py <in_manifest> <out_manifest> <channel> <version>"
        )

    in_manifest, out_manifest, channel, version = sys.argv[1:]
    src = Path(in_manifest)
    dst = Path(out_manifest)

    _log(
        f"args in_manifest={src} out_manifest={dst} channel={channel} version={version}"
    )

    if not src.is_file():
        raise SystemExit(f"input manifest not found: {src}")

    raw = src.read_text(encoding="utf-8")
    data = _load_json_tolerant(raw)
    _log(f"selected_object_type={type(data).__name__}")
    if isinstance(data, dict):
        _log(f"selected_object_keys={sorted(list(data.keys()))}")
    else:
        _log("selected object is not dict; downstream processing will fail")

    entries = data.get("native_artifacts", [])
    _log(f"native_artifacts_type={type(entries).__name__}")
    if not isinstance(entries, list):
        entries = []
    total_entries = len(entries)
    filtered = [
        e for e in entries if isinstance(e, dict) and e.get("channel") == channel
    ]
    data["native_artifacts"] = filtered
    _log(
        f"filter_result total_entries={total_entries}"
        f" filtered_entries={len(filtered)}"
        f" channel={channel}"
    )
    data["version"] = version
    _log(f"override_version={version}")
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    _log(f"write_ok out_manifest={dst}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
