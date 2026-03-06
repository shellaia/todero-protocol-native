#!/usr/bin/env python3
"""Validate alias-current and historical installability invariants on S3."""

from __future__ import annotations

import argparse
import gzip
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import Dict, List, Optional, Sequence, Set, Tuple


@dataclass
class ValidationError:
    code: str
    message: str
    context: Dict[str, str]

    def render(self) -> str:
        parts = [f"ERROR[{self.code}] {self.message}"]
        if self.context:
            ctx = " ".join(f"{k}={v}" for k, v in self.context.items())
            parts.append(f"| {ctx}")
        return " ".join(parts)


def _run(cmd: Sequence[str]) -> str:
    proc = subprocess.run(cmd, check=False, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}"
        )
    return proc.stdout


def _run_bytes(cmd: Sequence[str]) -> bytes:
    proc = subprocess.run(cmd, check=False, capture_output=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n"
            f"stdout:\n{proc.stdout.decode('utf-8', errors='replace')}\n"
            f"stderr:\n{proc.stderr.decode('utf-8', errors='replace')}"
        )
    return proc.stdout


def _s3_uri(bucket: str, key: str) -> str:
    return f"s3://{bucket}/{key.lstrip('/')}"


def _head_object(bucket: str, key: str) -> None:
    _run(["aws", "s3api", "head-object", "--bucket", bucket, "--key", key])


def _get_text(bucket: str, key: str) -> str:
    out = _run_bytes(["aws", "s3", "cp", _s3_uri(bucket, key), "-"])
    return out.decode("utf-8")


def _get_bytes(bucket: str, key: str) -> bytes:
    return _run_bytes(["aws", "s3", "cp", _s3_uri(bucket, key), "-"])


def _parse_packages_versions(text: str, package_name: str) -> Set[str]:
    versions: Set[str] = set()
    current_pkg: Optional[str] = None
    for line in text.splitlines():
        if line.startswith("Package: "):
            current_pkg = line.split(":", 1)[1].strip()
        elif line.startswith("Version: ") and current_pkg == package_name:
            versions.add(line.split(":", 1)[1].strip())
        elif not line.strip():
            current_pkg = None
    return versions


def _read_apt_packages(bucket: str, key: str) -> str:
    if key.endswith(".gz"):
        raw = _get_bytes(bucket, key)
        return gzip.decompress(raw).decode("utf-8")
    return _get_text(bucket, key)


def _join(prefix: str, *parts: str) -> str:
    p = PurePosixPath(prefix)
    for part in parts:
        p = p / part
    return str(p).lstrip("/")


def _load_bucket_manifest(bucket: str, prefix: str) -> Dict:
    key = _join(prefix, "releases", "manifest.json")
    data = json.loads(_get_text(bucket, key))
    if not isinstance(data, dict):
        raise ValueError(f"bucket manifest root is not object: {_s3_uri(bucket, key)}")
    if not isinstance(data.get("releases"), list):
        raise ValueError(f"bucket manifest releases is not list: {_s3_uri(bucket, key)}")
    return data


def _extract_primary_href(repomd_xml: str) -> Optional[str]:
    root = ET.fromstring(repomd_xml)
    ns = {"r": "http://linux.duke.edu/metadata/repo"}
    for data in root.findall("r:data", ns):
        if data.attrib.get("type") != "primary":
            continue
        loc = data.find("r:location", ns)
        if loc is not None and loc.attrib.get("href"):
            return loc.attrib["href"]
    return None


def _parse_yum_primary_versions(primary_xml: bytes, package_name: str) -> Set[str]:
    try:
        xml_text = gzip.decompress(primary_xml).decode("utf-8")
    except OSError:
        xml_text = primary_xml.decode("utf-8")
    root = ET.fromstring(xml_text)
    ns = {"m": "http://linux.duke.edu/metadata/common"}
    versions: Set[str] = set()
    for pkg in root.findall("m:package", ns):
        name = pkg.findtext("m:name", default="", namespaces=ns)
        if name != package_name:
            continue
        ver = pkg.find("m:version", ns)
        if ver is not None and ver.attrib.get("ver"):
            versions.add(ver.attrib["ver"])
    return versions


def _parse_brew_formula(text: str) -> Tuple[Optional[str], Optional[str]]:
    url = None
    sha = None
    for line in text.splitlines():
        m_url = re.match(r'^\s*url\s+"([^"]+)"\s*$', line)
        if m_url and url is None:
            url = m_url.group(1).strip()
        m_sha = re.match(r'^\s*sha256\s+"([^"]+)"\s*$', line)
        if m_sha and sha is None:
            sha = m_sha.group(1).strip().lower()
    return url, sha


def _validate_apt_alias_current_only(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    current_version: str,
) -> None:
    versions: Set[str] = set()
    for arch in ("amd64", "arm64"):
        for suffix in ("Packages", "Packages.gz"):
            key = _join(
                prefix,
                "channels",
                "stable",
                "dists",
                "stable",
                "main",
                f"binary-{arch}",
                suffix,
            )
            try:
                content = _read_apt_packages(bucket, key)
            except Exception as exc:
                errors.append(
                    ValidationError(
                        "APT_ALIAS_METADATA_MISSING",
                        "failed reading apt alias metadata",
                        {"arch": arch, "key": key, "error": str(exc)},
                    )
                )
                continue
            parsed = _parse_packages_versions(content, "todero-native")
            if not parsed:
                errors.append(
                    ValidationError(
                        "APT_ALIAS_PACKAGE_NOT_FOUND",
                        "todero-native not present in apt alias metadata",
                        {"arch": arch, "key": key},
                    )
                )
            versions.update(parsed)
    if versions != {current_version}:
        errors.append(
            ValidationError(
                "APT_ALIAS_NOT_CURRENT_ONLY",
                "apt alias must expose exactly one effective current version",
                {"expected": current_version, "found": ",".join(sorted(versions)) or "<none>"},
            )
        )


def _validate_yum_alias_current_only(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    current_version: str,
) -> None:
    base = _join(prefix, "channels", "stable")
    repomd_key = _join(base, "repodata", "repomd.xml")
    try:
        repomd = _get_text(bucket, repomd_key)
    except Exception as exc:
        errors.append(
            ValidationError(
                "YUM_ALIAS_REPOMD_MISSING",
                "failed reading yum alias repomd.xml",
                {"key": repomd_key, "error": str(exc)},
            )
        )
        return

    try:
        href = _extract_primary_href(repomd)
    except Exception as exc:
        errors.append(
            ValidationError(
                "YUM_ALIAS_REPOMD_PARSE_FAILED",
                "failed parsing yum alias repomd.xml",
                {"key": repomd_key, "error": str(exc)},
            )
        )
        return

    if not href:
        errors.append(
            ValidationError(
                "YUM_ALIAS_PRIMARY_NOT_FOUND",
                "yum alias repomd.xml missing primary metadata reference",
                {"key": repomd_key},
            )
        )
        return

    primary_key = _join(base, href)
    try:
        primary_raw = _get_bytes(bucket, primary_key)
    except Exception as exc:
        errors.append(
            ValidationError(
                "YUM_ALIAS_PRIMARY_READ_FAILED",
                "failed downloading yum alias primary metadata",
                {"key": primary_key, "error": str(exc)},
            )
        )
        return

    try:
        versions = _parse_yum_primary_versions(primary_raw, "todero-native")
    except Exception as exc:
        errors.append(
            ValidationError(
                "YUM_ALIAS_PRIMARY_PARSE_FAILED",
                "failed parsing yum alias primary metadata",
                {"key": primary_key, "error": str(exc)},
            )
        )
        return

    if versions != {current_version}:
        errors.append(
            ValidationError(
                "YUM_ALIAS_NOT_CURRENT_ONLY",
                "yum alias must expose exactly one effective current version",
                {"expected": current_version, "found": ",".join(sorted(versions)) or "<none>"},
            )
        )


def _validate_brew_alias_current_only(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    current_version: str,
) -> None:
    base = _join(prefix, "channels", "stable")
    formula_key = _join(base, "todero-native.rb")
    try:
        formula = _get_text(bucket, formula_key)
    except Exception as exc:
        errors.append(
            ValidationError(
                "BREW_ALIAS_FORMULA_MISSING",
                "failed reading brew alias formula",
                {"key": formula_key, "error": str(exc)},
            )
        )
        return

    url, sha = _parse_brew_formula(formula)
    if not url or not sha:
        errors.append(
            ValidationError(
                "BREW_ALIAS_FORMULA_PARSE_FAILED",
                "brew alias formula missing url/sha256 entries",
                {"key": formula_key},
            )
        )
        return

    expected_archive = f"todero-native-darwin-aarch64-{current_version}.tar.gz"
    expected_url = f"https://brew.social100.com/{prefix}/channels/stable/{expected_archive}"
    if url != expected_url:
        errors.append(
            ValidationError(
                "BREW_ALIAS_URL_MISMATCH",
                "brew alias formula url must target current stable alias archive",
                {"expected": expected_url, "found": url},
            )
        )

    checksum_key = _join(base, f"{expected_archive}.sha256")
    try:
        sha_text = _get_text(bucket, checksum_key)
    except Exception as exc:
        errors.append(
            ValidationError(
                "BREW_ALIAS_SHA_MISSING",
                "brew alias checksum file missing/unreadable",
                {"key": checksum_key, "error": str(exc)},
            )
        )
        return

    declared = sha_text.strip().split()[0].lower() if sha_text.strip() else ""
    if not declared:
        errors.append(
            ValidationError(
                "BREW_ALIAS_SHA_INVALID",
                "brew alias checksum file is empty/invalid",
                {"key": checksum_key},
            )
        )
        return
    if declared != sha:
        errors.append(
            ValidationError(
                "BREW_ALIAS_SHA_MISMATCH",
                "brew alias formula sha256 does not match alias checksum",
                {"formula_sha": sha, "alias_sha": declared},
            )
        )


def _validate_apt_snapshot_versions(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    versions: Sequence[str],
) -> None:
    for version in versions:
        base = _join(prefix, "releases", version)
        for key in (
            _join(base, "dists", "stable", "InRelease"),
            _join(base, "dists", "stable", "Release"),
            _join(base, "dists", "stable", "Release.gpg"),
            _join(base, "dists", "stable", "main", "binary-amd64", "Packages"),
            _join(base, "dists", "stable", "main", "binary-amd64", "Packages.gz"),
            _join(base, "dists", "stable", "main", "binary-arm64", "Packages"),
            _join(base, "dists", "stable", "main", "binary-arm64", "Packages.gz"),
        ):
            try:
                _head_object(bucket, key)
            except Exception as exc:
                errors.append(
                    ValidationError(
                        "APT_SNAPSHOT_OBJECT_MISSING",
                        "required apt snapshot object missing",
                        {"version": version, "key": key, "error": str(exc)},
                    )
                )

        for deb_name in (
            f"todero-native_{version}_amd64.deb",
            f"todero-native_{version}_arm64.deb",
        ):
            key = _join(base, "pool", "main", "t", "todero-native", deb_name)
            try:
                _head_object(bucket, key)
            except Exception as exc:
                errors.append(
                    ValidationError(
                        "APT_SNAPSHOT_DEB_MISSING",
                        "apt snapshot missing expected deb package",
                        {"version": version, "key": key, "error": str(exc)},
                    )
                )

        for arch in ("amd64", "arm64"):
            key = _join(
                base,
                "dists",
                "stable",
                "main",
                f"binary-{arch}",
                "Packages",
            )
            try:
                content = _read_apt_packages(bucket, key)
            except Exception as exc:
                errors.append(
                    ValidationError(
                        "APT_SNAPSHOT_PACKAGES_READ_FAILED",
                        "failed reading apt snapshot Packages",
                        {"version": version, "arch": arch, "key": key, "error": str(exc)},
                    )
                )
                continue
            parsed = _parse_packages_versions(content, "todero-native")
            if parsed != {version}:
                errors.append(
                    ValidationError(
                        "APT_SNAPSHOT_VERSION_MISMATCH",
                        "apt snapshot metadata must expose only its own version",
                        {
                            "version": version,
                            "arch": arch,
                            "found": ",".join(sorted(parsed)) or "<none>",
                        },
                    )
                )


def _validate_yum_snapshot_versions(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    versions: Sequence[str],
) -> None:
    for version in versions:
        base = _join(prefix, "releases", version)
        repomd_key = _join(base, "repodata", "repomd.xml")
        try:
            repomd = _get_text(bucket, repomd_key)
        except Exception as exc:
            errors.append(
                ValidationError(
                    "YUM_REPOMD_MISSING",
                    "yum snapshot repomd.xml missing/unreadable",
                    {"version": version, "key": repomd_key, "error": str(exc)},
                )
            )
            continue

        try:
            href = _extract_primary_href(repomd)
        except Exception as exc:
            errors.append(
                ValidationError(
                    "YUM_REPOMD_PARSE_FAILED",
                    "failed parsing yum repomd.xml",
                    {"version": version, "error": str(exc)},
                )
            )
            href = None

        if not href:
            errors.append(
                ValidationError(
                    "YUM_PRIMARY_NOT_FOUND",
                    "yum repomd.xml missing primary metadata reference",
                    {"version": version},
                )
            )
            continue

        primary_key = _join(base, href)
        try:
            primary_raw = _get_bytes(bucket, primary_key)
        except Exception as exc:
            errors.append(
                ValidationError(
                    "YUM_PRIMARY_READ_FAILED",
                    "failed downloading yum primary metadata",
                    {"version": version, "key": primary_key, "error": str(exc)},
                )
            )
            continue

        try:
            pkg_versions = _parse_yum_primary_versions(primary_raw, "todero-native")
        except Exception as exc:
            errors.append(
                ValidationError(
                    "YUM_PRIMARY_PARSE_FAILED",
                    "failed parsing yum primary metadata",
                    {"version": version, "key": primary_key, "error": str(exc)},
                )
            )
            pkg_versions = set()

        if version not in pkg_versions:
            errors.append(
                ValidationError(
                    "YUM_SNAPSHOT_VERSION_MISSING",
                    "yum snapshot metadata does not expose requested version",
                    {"version": version, "found": ",".join(sorted(pkg_versions)) or "<none>"},
                )
            )

        for rpm in (
            f"todero-native-{version}-1.x86_64.rpm",
            f"todero-native-{version}-1.aarch64.rpm",
        ):
            key = _join(base, "packages", rpm)
            try:
                _head_object(bucket, key)
            except Exception as exc:
                errors.append(
                    ValidationError(
                        "YUM_SNAPSHOT_RPM_MISSING",
                        "yum snapshot missing expected rpm package",
                        {"version": version, "key": key, "error": str(exc)},
                    )
                )


def _validate_brew_snapshot_versions(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
    versions: Sequence[str],
) -> None:
    for version in versions:
        base = _join(prefix, "releases", version)
        formula_key = _join(base, "todero-native.rb")
        try:
            formula = _get_text(bucket, formula_key)
        except Exception as exc:
            errors.append(
                ValidationError(
                    "BREW_FORMULA_MISSING",
                    "brew snapshot formula missing/unreadable",
                    {"version": version, "key": formula_key, "error": str(exc)},
                )
            )
            continue

        url, sha = _parse_brew_formula(formula)
        if not url or not sha:
            errors.append(
                ValidationError(
                    "BREW_FORMULA_PARSE_FAILED",
                    "brew formula missing url/sha256 entries",
                    {"version": version, "key": formula_key},
                )
            )
            continue

        expected_archive = f"todero-native-darwin-aarch64-{version}.tar.gz"
        if expected_archive not in url:
            errors.append(
                ValidationError(
                    "BREW_FORMULA_URL_VERSION_MISMATCH",
                    "brew snapshot formula url does not reference same-version archive",
                    {"version": version, "url": url},
                )
            )

        checksum_key = _join(base, f"{expected_archive}.sha256")
        try:
            sha_text = _get_text(bucket, checksum_key)
        except Exception as exc:
            errors.append(
                ValidationError(
                    "BREW_SNAPSHOT_SHA_MISSING",
                    "brew snapshot checksum file missing/unreadable",
                    {"version": version, "key": checksum_key, "error": str(exc)},
                )
            )
            continue

        declared = sha_text.strip().split()[0].lower() if sha_text.strip() else ""
        if not declared:
            errors.append(
                ValidationError(
                    "BREW_SNAPSHOT_SHA_INVALID",
                    "brew snapshot checksum file is empty/invalid",
                    {"version": version, "key": checksum_key},
                )
            )
            continue
        if declared != sha:
            errors.append(
                ValidationError(
                    "BREW_FORMULA_SHA_MISMATCH",
                    "brew snapshot formula sha256 does not match snapshot checksum",
                    {"version": version, "formula_sha": sha, "snapshot_sha": declared},
                )
            )


def _manifest_versions_or_error(
    errors: List[ValidationError],
    bucket: str,
    prefix: str,
) -> List[str]:
    try:
        manifest = _load_bucket_manifest(bucket, prefix)
    except Exception as exc:
        errors.append(
            ValidationError(
                "MANIFEST_LOAD_FAILED",
                "failed loading channel bucket manifest",
                {"bucket": bucket, "prefix": prefix, "error": str(exc)},
            )
        )
        return []
    releases = manifest.get("releases", [])
    versions = [r.get("version", "") for r in releases if isinstance(r, dict)]
    versions = [v for v in versions if v]
    if not versions:
        errors.append(
            ValidationError(
                "MANIFEST_EMPTY",
                "channel bucket manifest has no releases",
                {"bucket": bucket, "prefix": prefix},
            )
        )
    return versions


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate alias-current and historical installability invariants."
    )
    parser.add_argument("--bucket-apt", required=True)
    parser.add_argument("--bucket-yum", required=True)
    parser.add_argument("--bucket-brew", required=True)
    parser.add_argument("--prefix", required=True, help="S3_PREFIX value")
    parser.add_argument("--current-version", required=True, help="Current release version (no leading v)")
    parser.add_argument("--history-limit", type=int, default=0, help="Optional cap of versions validated per channel (0=all)")
    args = parser.parse_args()

    prefix = args.prefix.strip().strip("/")
    current = args.current_version.strip()

    errors: List[ValidationError] = []

    _validate_apt_alias_current_only(errors, args.bucket_apt, prefix, current)
    _validate_yum_alias_current_only(errors, args.bucket_yum, prefix, current)
    _validate_brew_alias_current_only(errors, args.bucket_brew, prefix, current)

    apt_versions = _manifest_versions_or_error(errors, args.bucket_apt, prefix)
    yum_versions = _manifest_versions_or_error(errors, args.bucket_yum, prefix)
    brew_versions = _manifest_versions_or_error(errors, args.bucket_brew, prefix)

    if args.history_limit > 0:
        apt_versions = apt_versions[: args.history_limit]
        yum_versions = yum_versions[: args.history_limit]
        brew_versions = brew_versions[: args.history_limit]

    _validate_apt_snapshot_versions(errors, args.bucket_apt, prefix, apt_versions)
    _validate_yum_snapshot_versions(errors, args.bucket_yum, prefix, yum_versions)
    _validate_brew_snapshot_versions(errors, args.bucket_brew, prefix, brew_versions)

    if errors:
        for err in errors:
            print(err.render(), file=sys.stderr)
        print(f"validation_failed count={len(errors)}", file=sys.stderr)
        return 1

    print(
        "validation_passed "
        f"apt_versions={len(apt_versions)} yum_versions={len(yum_versions)} brew_versions={len(brew_versions)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
