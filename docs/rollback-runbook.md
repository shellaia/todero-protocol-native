# Todero Native Rollback Runbook (Alias Repoint Only)

## Scope
Rollback for `todero-protocol-native` is alias repoint only.  
Snapshots under `releases/<version>/` are immutable and must never be deleted during rollback.

## Pre-Checks (Mandatory)
1. Identify rollback target version per channel (`apt`, `yum`, `brew`).
2. Validate candidate snapshot completeness before any alias change:

```bash
scripts/rollback_alias_repoint.sh \
  --channel apt \
  --bucket <S3_BUCKET_APT> \
  --prefix <S3_PREFIX> \
  --version <TARGET_VERSION>
```

Repeat for `yum` and `brew`.  
Default mode is non-destructive dry-run; it fails if required snapshot objects are missing.

## Apply Rollback (Explicit)
After pre-checks pass, apply alias repoint:

```bash
scripts/rollback_alias_repoint.sh \
  --channel apt \
  --bucket <S3_BUCKET_APT> \
  --prefix <S3_PREFIX> \
  --version <TARGET_VERSION> \
  --apply --yes
```

Repeat for `yum` and `brew`.

## Post-Apply Verification Commands
1. Confirm alias contains target manifest:

```bash
aws s3api head-object \
  --bucket <BUCKET> \
  --key "<S3_PREFIX>/todero-release-manifest-<TARGET_VERSION>.json"
```

2. Confirm channel-critical metadata exists:
- APT alias:
  - `<S3_PREFIX>/channels/stable/dists/stable/InRelease`
  - `<S3_PREFIX>/channels/stable/dists/stable/Release`
  - `<S3_PREFIX>/channels/stable/dists/stable/Release.gpg`
- YUM alias:
  - `<S3_PREFIX>/repodata/repomd.xml`
  - `<S3_PREFIX>/repodata/repomd.xml.asc`
- BREW alias:
  - `<S3_PREFIX>/todero-native.rb`

3. Trigger/verify CloudFront metadata invalidation for alias paths after rollback.

## CI Dry-Drill
Release workflow includes a non-destructive rollback dry-drill step that:
- picks a candidate rollback version from each bucket manifest (previous if available, otherwise current),
- runs `scripts/rollback_alias_repoint.sh` in dry-run mode for `apt`, `yum`, `brew`,
- fails on snapshot incompleteness.
