# CI and releases

Cito supports direct development on `main`. Pull requests are optional. GitHub
Actions performs continuous checks on source changes and publishes a release only
when an annotated stable version tag is selected.

## Two workflows

| Workflow | Triggers | Effects |
| --- | --- | --- |
| `.github/workflows/ci.yml` | Push to `main`, pull request, manual run | Checks, tests, arm64 build, and an ad-hoc-signed candidate artifact |
| `.github/workflows/release.yml` | Push of `v*`, manual run for an existing tag | Developer ID signing, notarization, GitHub Release, install verification, and Homebrew tap update |

The website workflow remains independent. A normal `main` push never publishes
an application release, runs notarization, or updates the tap. Local development
on multiple Macs needs no Developer ID private key when formal signing happens
in Actions.

## Checks and candidates

CI uses a macOS arm64 runner, the Rust toolchain in `rust-toolchain.toml`,
Python 3.13, and Node.js 24. Python is release tooling; Node is used only for the
existing dependency-free Pi extension tests. Neither becomes an installed Cito
runtime dependency.

CI runs:

```sh
python3 packaging/release.py check
python3 -m unittest discover -s packaging -p 'test_*.py' -v
/usr/bin/ruby -c packaging/homebrew/cito.rb
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=4
cargo build --release --locked
python3 packaging/release.py bundle build/candidate.zip
```

Tests remain headless and sandboxed. CI verifies bundle versions, signatures,
resources, and the packaged CLI without starting the desktop UI or a real login
item. Cargo downloads are cached. The build directory remains incremental within
a job; it is not restored independently of the post-link staged binaries.

A passing run uploads `cito-candidate-<commit-sha>` for 14 days. This artifact is
ad-hoc signed and has not been notarized. Formal release reuse accepts only a
passing **push-to-main** CI run for the exact selected source commit. It does not
select the latest artifact by branch name or use a pull-request artifact.

If matching CI is still running, release waits up to 20 minutes. Failed CI blocks
release. Missing, expired, or cancelled candidates cause the same source commit
to be checked and rebuilt; release never substitutes a newer `main` commit.

## Version preparation on main

`crates/cito-native/Cargo.toml` is the canonical product version. Prepare a normal
commit on `main` updating:

- The native crate version and the native package entry in `Cargo.lock`.
- `.agents/plugins/marketplace.json`, including its nested plugin version.
- Distribution manifests in `plugins/`, including the Pi package manifest and the
  Codex/Claude marketplace manifests. `packaging/release.py check` lists drift.
- The `application-version` metadata in the four website HTML pages, plus user
  documentation and release notes where they describe the current version.

Update the lockfile with Cargo after changing the package version; preserve
locked dependency versions. CI uses `--locked` and checks all distribution copies
against the canonical crate version. Tests obtain the expected runtime version
from `CARGO_PKG_VERSION`.

`packaging/homebrew/cito.rb` is a **template**, not an installable development
Cask. It contains `__VERSION__` and `__SHA256__` tokens. Do not invent a future
archive checksum or copy a local-build checksum into the source tree. Release
fills both fields only after the signed, notarized, stapled ZIP is final. The tap
stores the concrete installable Cask.

Push the version commit, wait for its CI, then tag that exact commit. For example,
if the canonical version has been updated to `0.21.3`:

```sh
git push origin main
# Wait for CI to pass for this exact commit.
git tag -a v0.21.3 -m "Release v0.21.3"
git push origin v0.21.3
```

The release workflow requires:

- A stable `vMAJOR.MINOR.PATCH` tag without leading zeroes.
- An annotated tag pointing to the checked-out commit.
- That commit to be reachable from `origin/main`.
- The tag version to match the native crate and distribution metadata.

Prerelease channels are not configured. Do not move an existing tag or replace
assets of an existing published version. Preparation, tagging, and runtime
configuration remain separate from internal Vectl phase and SQLite schema
versions.

## Signing and notarization credentials

Configure these **Actions repository secrets** in `Tefx/cito`:

| Secret | Purpose |
| --- | --- |
| `APPLE_CERTIFICATE_P12_BASE64` | Base64 representation of the exported Developer ID Application certificate and private key |
| `APPLE_CERTIFICATE_PASSWORD` | Password protecting that P12 export |
| `APPLE_SIGNING_IDENTITY` | Full `Developer ID Application: ... (TEAMID)` identity name |
| `APPLE_ID` | Apple Account email used for notarization |
| `APPLE_TEAM_ID` | Developer team identifier matching the signing certificate |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password for the notarization client |
| `HOMEBREW_TAP_SSH_KEY` | Dedicated, non-passphrase-protected SSH deploy key with write access only to the declared tap |

The declared tap is `Tefx/homebrew-tap`. Its public deploy key must have **Allow
write access** enabled. The default repository `GITHUB_TOKEN` cannot push to that
other repository. Do not copy a personal `gh` login token into Actions as a
shortcut. Keep an independent encrypted P12 backup for device replacement.

CI jobs receive none of these credentials. Release scopes Apple credentials to
the signing step and the SSH key to tap preflight/push steps. Passwords travel
through standard input or native hidden prompts, not command arguments. The
release producer suppresses credential-bearing native diagnostics.

Signing uses a temporary keychain added to the user keychain search list. That
search-list step is required for private-key lookup, even when codesign also
receives an explicit keychain. Import grants access to the signing tools, and
cleanup restores the original search list and removes the temporary keychain
and exported key files. The runner is ephemeral.

## Release sequence

1. Validate the tag and metadata; require the scoped tap credential before any
   publication.
2. On retry, inspect existing GitHub Release assets. Reuse a ZIP only after
   checking its checksum, packaged version, signature, and stapled ticket.
3. Otherwise obtain the exact-commit CI candidate, or check and rebuild that
   source commit when no reusable candidate exists.
4. Import the signing identity, validate Apple notarization credentials, and sign
   all Mach-O files and nested code bundles from the inside out. Sign the outer
   App last. `--deep` is used for verification, not release signing.
5. Submit the signed App for notarization and wait up to 20 minutes. Continue only
   on `Accepted`; staple and validate the ticket.
6. Create `Cito-macos-arm64.zip` and its `.sha256` sidecar. Compute the checksum
   after signing and stapling. Do not modify or repackage the App afterward.
7. Upload to a draft GitHub Release and publish after the expected assets exist.
   An older release does not replace a newer version as GitHub's latest release.
8. Generate the Cask from that final checksum. Test the actual public download
   through Homebrew before pushing the tap.
9. Push the verified Cask and read back the tap's `main` commit. Never force-push
   or downgrade a newer tap version.

Release runs are serialized and never automatically cancelled by a newer run.
Ordinary CI may cancel superseded runs. There is no automatic unsigned fallback
when release signing or notarization credentials are missing.

The Homebrew test uses an isolated home/state and app directory. It verifies an
install **without `--cask`**, both command symlinks and their versions, ordinary
uninstall cleanup, and preservation of user data. It never launches the UI or
uses `--zap`. It restores the runner's local tap files afterward.

GitHub Release publication and the tap push are separate transactions. The public
Release can exist while the tap job fails. A later retry reuses those immutable
assets instead of signing/notarizing again. Install verification uses the public
release URL, not a locally substituted ZIP.

## Retrying and recovery

For a tag created after this workflow was introduced, retry with its own workflow
revision and explicit tag:

```sh
gh workflow run release.yml --repo Tefx/cito --ref v0.21.3 -f tag=v0.21.3
```

Tags predating this workflow are not supported by this command. A workflow run
on a branch cannot stand in for an explicitly selected tag.

- **CI or candidate failure:** fix source through a new commit and version as
  appropriate. An actual failing CI result is not bypassed by rebuilding.
- **Notarization rejection:** inspect the Apple submission log. Correct the
  cause before another submission.
- **Notarization timeout or interrupted submission:** inspect authenticated
  `notarytool history` and the submission log first. Do not blindly rerun or
  assume a nonzero command exit means Apple received nothing. The workflow does
  not automatically resubmit an uncertain request.
- **Draft upload failure:** inspect the draft assets. A complete ZIP is reused;
  its missing checksum can be reconstructed in a draft. Unexpected partial or
  mismatched state stops the workflow. No `--clobber` is used.
- **Published release / tap failure:** fix the tap credential, install failure,
  or concurrent push issue, then retry the same tag. Existing published payloads
  are not replaced.
- **Defective published version:** publish a new patch. If withdrawing a version,
  reconcile the Release and tap separately. Moving the tap back does not
  automatically downgrade installations that already upgraded.

Use authenticated native tools to inspect detailed Apple errors locally. Never
paste passwords, private keys, or unredacted authentication logs into issues or
agent messages.

## Consumer and agent guidance

Use the Homebrew Cask for application and CLI ownership:

```sh
brew install tefx/tap/cito
brew upgrade --cask tefx/tap/cito
brew uninstall --cask tefx/tap/cito
```

Do not restore a same-name Formula, write unmanaged links into the Homebrew
prefix, or depend on a versioned Cellar/App path. Keep CLI consumers on the
Homebrew-managed `cito` / `cito-mcp` links. Explicit `--cask` remains supported.
Use immutable semantic release tags for non-Homebrew MCP/deployment references;
do not point production consumers at `main` or a raw commit as a substitute for
a release. An ad-hoc candidate is intended for testing, not a signed release.

The first Developer ID rollout and later changes to signing identity,
entitlements, or launch packaging need a manual first-launch check on a clean
Mac. Automated bundle/install checks do not establish Gatekeeper first-launch
behavior or replace a full App notarization acceptance.
