# Distribution Pattern

## Name
`distribution-public-channel-private-origin`

## Intent
Define one clear distribution pattern for `todero-protocol-native` where:
- user-facing install/download endpoints are always public channel domains,
- CI publish/update operations always target private S3 origins,
- release history and current alias are separated and deterministic.

This pattern is scoped only to distribution behavior.

## Rules
1. Public URL rule (client-facing):
   - Brew formula URLs must use `https://brew.social100.com/<S3_PREFIX>/...`.
   - APT install docs must use `https://apt.social100.com/<S3_PREFIX>/...`.
   - YUM install docs must use `https://yum.social100.com/<S3_PREFIX>/...`.
   - Do not expose direct `s3.amazonaws.com` URLs in install-facing formula/docs.

2. Private origin rule (pipeline-facing):
   - Release workflow publishes artifacts and metadata to S3 buckets.
   - Bucket writes and validations use `s3://<bucket>/<S3_PREFIX>/...` paths.
   - CloudFront/public domains are delivery layers, not publish targets.

3. Alias/snapshot rule:
   - Immutable history: `.../releases/<version>/...`
   - Current alias:
     - APT: `.../channels/stable/...`
     - YUM/Brew: `.../<S3_PREFIX>/...`
   - Alias represents current only; history is preserved in release snapshots + manifest.
   - Alias publish must be pruning/synchronizing (for example `aws s3 sync <stage> <alias> --delete`) so stale files from previous releases cannot remain in alias paths.

4. Manifest rule:
   - Keep `.../releases/manifest.json` per channel bucket.
   - Manifest must include release version, publishedAt, snapshot, alias.
   - Manifest must be updated after snapshot + alias publish.

5. Ownership rule:
   - `todero-protocol-native` is sole owner of `todero-native` distribution outputs.
   - Brew formula publisher target is `shellaia/homebrew-todero`.

## Current vs Historical Install Behavior
This pattern intentionally separates default installs from historical installs.

### APT
- Alias (`https://apt.social100.com/<S3_PREFIX>/channels/stable`) is current-only.
- `apt install todero-native` resolves to current version from alias metadata.
- Historical install is still supported through snapshot repo metadata:
  1. Add snapshot source for a specific release:
     - `deb [signed-by=/usr/share/keyrings/todero-native-archive-keyring.gpg] https://apt.social100.com/<S3_PREFIX>/releases/<VERSION> stable main`
  2. `sudo apt update`
  3. Install pinned version:
     - `sudo apt install todero-native=<VERSION>`
- If only alias is configured, `apt install todero-native=<VERSION>` works only when `<VERSION>` equals current alias version.

### YUM/DNF
- Alias (`https://yum.social100.com/<S3_PREFIX>`) is current-only.
- Historical installs use snapshot repo path:
  - `https://yum.social100.com/<S3_PREFIX>/releases/<VERSION>/`

### Brew
- Alias formula (`https://brew.social100.com/<S3_PREFIX>/todero-native.rb`) is current-only.
- Historical installs use snapshot formula URL:
  - `https://brew.social100.com/<S3_PREFIX>/releases/<VERSION>/todero-native.rb`

## Required CI Inputs
- Secrets:
  - `S3_BUCKET_APT`, `S3_BUCKET_YUM`, `S3_BUCKET_BREW`, `S3_PREFIX`
  - signing keys/passphrases as required by apt/yum stages
- Variables/secrets for Brew tap auth:
  - `BREW_TAP_REPO` (variable)
  - GitHub App credentials for tap publish token minting

## Validation Checklist
1. Generated brew formula URL uses `brew.social100.com` and not direct S3 URL.
2. APT/YUM/Brew artifacts exist in snapshot paths (`releases/<version>`).
3. Alias paths resolve and point to current release payload.
   - Alias roots must not retain stale artifacts from older releases after publish.
4. `releases/manifest.json` includes current version entry with correct snapshot/alias.
5. Brew tap formula in `shellaia/homebrew-todero` matches generated formula.
6. APT alias-current and history-intact check (S3-only validation):
   - Validate alias metadata under `<S3_PREFIX>/channels/stable/...` exposes only one effective current version.
   - Validate alias points to current release payload only.
   - Validate all historical versions remain intact under `<S3_PREFIX>/releases/<version>/...` with full apt repo structure (`InRelease`, `Release`, `Release.gpg`, `Packages*`, `pool/...`).
   - Do not require package installation for this check; inspect S3 objects + repo metadata only.
7. Installability outcome check (policy-level):
   - Latest/current is installable via alias path.
   - Older versions remain installable via versioned snapshot repo paths.

## Non-Goals
- Does not define auth/identity patterns.
- Does not define gameplay/runtime component behavior.
- Does not define packaging internals outside distribution contract.
