#!/usr/bin/env python3
"""Build, sign, publish, and install-test Cito's tracked release artifacts."""

import argparse
import base64
import contextlib
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = "Cito-macos-arm64.zip"
VERSION_FILES = (
    ".agents/plugins/marketplace.json",
    "plugins/codex/.codex-plugin/plugin.json",
    "plugins/claude/.claude-plugin/plugin.json",
    "plugins/pi/package.json",
    "plugins/pi/plugin.json",
    "plugins/codex/plugin.json",
    "plugins/claude/plugin.json",
    "plugins/human/plugin.json",
    "plugins/telegram/plugin.json",
)
MACHO_MAGIC = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
    b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
}


class ReleaseError(Exception):
    pass


def version_tuple(value):
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value):
        raise ReleaseError("Stable releases require a MAJOR.MINOR.PATCH version.")
    return tuple(map(int, value.split(".")))


def version(root=ROOT):
    value = tomllib.loads((root / "crates/cito-native/Cargo.toml").read_text())["package"]["version"]
    version_tuple(value)
    return value


def check_metadata(root=ROOT):
    expected = version(root)
    packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    if [p["version"] for p in packages if p["name"] == "cito-native"] != [expected]:
        raise ReleaseError("Cargo.lock does not match the native crate version.")
    for name in VERSION_FILES:
        data = json.loads((root / name).read_text())
        versions = [data.get("version")]
        if name.endswith("marketplace.json"):
            versions += [p.get("version") for p in data["plugins"]]
        if any(v != expected for v in versions):
            raise ReleaseError(f"Distribution version mismatch: {name}")
    for page in (root / "site").rglob("*.html"):
        values = re.findall(r'<meta name="application-version" content="([^"]+)"', page.read_text())
        if values != [expected]:
            raise ReleaseError(f"Website version missing or mismatched: {page.relative_to(root)}")
    return expected


def run(args, *, input=None, env=None, cwd=None, check=True, timeout=None):
    result = subprocess.run(
        list(map(str, args)), input=input, text=True, capture_output=True,
        env=env, cwd=cwd, timeout=timeout,
    )
    if check and result.returncode:
        # Native diagnostics may contain credential identifiers or private paths.
        raise ReleaseError(f"{Path(args[0]).name} failed with exit code {result.returncode}.")
    return result


def required(name):
    value = os.environ.get(name, "")
    if not value:
        raise ReleaseError(f"Required setting is missing: {name}")
    return value


def output(name, value):
    if path := os.environ.get("GITHUB_OUTPUT"):
        with open(path, "a") as stream:
            stream.write(f"{name}={value}\n")
    print(f"{name}={value}")


def render_cask(template, release_version, checksum):
    version_tuple(release_version)
    if not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ReleaseError("The archive SHA-256 must contain 64 lowercase hexadecimal characters.")
    for token in ("__VERSION__", "__SHA256__"):
        if template.count(token) != 1:
            raise ReleaseError(f"Cask template must contain exactly one {token} token.")
    return template.replace("__VERSION__", release_version).replace("__SHA256__", checksum)


def isolated_env(home):
    inherited = {name: os.environ[name] for name in ("PATH", "TMPDIR", "LANG", "LC_ALL", "CI") if name in os.environ}
    return {
        **inherited, "HOME": str(home), "CITO_STATE_DIR": str(home / ".cito"),
        "CITO_CONFIG_PATH": str(home / ".cito/config.json"),
        "CITO_DISABLE_UI": "1", "CITO_UI_HEADLESS": "1", "CITO_BANNER_HEADLESS": "1",
        "CITO_CODESIGN_IDENTITY": "", "APPLE_SIGNING_IDENTITY": "",
    }


def verify_bundle(app, expected):
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info.get("CFBundleShortVersionString") != expected or info.get("CFBundleVersion") != expected:
        raise ReleaseError("App bundle version does not match the release version.")
    if not (app / "Contents/Resources/plugins").is_dir():
        raise ReleaseError("App bundle is missing its bundled plugins.")
    for name in ("Contents/Helpers/cito-native", "Contents/Helpers/cito-ui", "Contents/Resources/AppIcon.icns"):
        if not (app / name).is_file():
            raise ReleaseError(f"App bundle is missing a required release component: {name}")
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app])
    with tempfile.TemporaryDirectory() as directory:
        result = run([app / "Contents/MacOS/Cito", "--version"], env=isolated_env(Path(directory)))
    if result.stdout.strip() != f"cito v{expected}":
        raise ReleaseError("Bundled CLI version does not match the release version.")
    for name in ("cito-native", "cito-ui"):
        if run(["/usr/bin/lipo", "-archs", app / "Contents/Helpers" / name]).stdout.strip() != "arm64":
            raise ReleaseError("The release requires arm64 native executables.")


def bundle(destination):
    expected = check_metadata()
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        run([ROOT / "bin/cito", "setup", "--build-app", root / "build", "--json"], env=isolated_env(home))
        app = root / "build/Cito.app"
        verify_bundle(app, expected)
        run(["/usr/bin/ditto", "-c", "-k", "--keepParent", app, destination])
    print("Candidate bundle and CLI verified.")


def signing_order(app):
    binaries = []
    bundles = []
    for path in app.rglob("*"):
        if path.is_symlink():
            continue
        if path.is_file():
            with path.open("rb") as stream:
                if stream.read(4) in MACHO_MAGIC:
                    binaries.append(path)
        elif path.suffix in (".app", ".framework", ".xpc", ".appex", ".bundle"):
            bundles.append(path)
    deepest_first = lambda p: (-len(p.parts), str(p))
    return sorted(binaries, key=deepest_first) + sorted(bundles, key=deepest_first) + [app]


def sign_bundle(app, identity, *, keychain=None, timestamp=True):
    for path in signing_order(app):
        args = ["/usr/bin/codesign", "--force", "--sign", identity, "--options", "runtime"]
        if keychain:
            args += ["--keychain", keychain]
        args += ["--timestamp" if timestamp else "--timestamp=none", path]
        run(args)
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app])


NOTARY_AUTH = r'''
log_user 0
set timeout 90
array set seen {key 0 apple 0 password 0 team 0}
proc answer {field value} {
    global seen spawn_id
    if {$seen($field)} {
        puts stderr "Native credential prompt repeated; refusing another authentication attempt."
        catch {exec kill -TERM [exp_pid]}
        catch {wait}
        exit 72
    }
    set seen($field) 1
    send -- "$value\r"
}
spawn -noecho /bin/bash --noprofile --norc -c {stty -echo; exec /usr/bin/xcrun notarytool store-credentials cito-release --keychain "$CITO_KEYCHAIN" --validate}
expect {
    -re {private key:} {answer key ""; exp_continue}
    -re {Apple ID:} {answer apple $env(APPLE_ID); exp_continue}
    -re {[Aa]pp-specific password[^:\r\n]*:} {answer password $env(APPLE_APP_SPECIFIC_PASSWORD); exp_continue}
    -re {Team ID[^:\r\n]*:} {answer team $env(APPLE_TEAM_ID); exp_continue}
    timeout {
        catch {exec kill -TERM [exp_pid]}
        catch {wait}
        puts stderr "Native credential validation timed out."
        exit 70
    }
    eof {}
}
set result [wait]
if {[lindex $result 2] != 0 || [lindex $result 3] != 0 || !$seen(apple) || !$seen(password) || !$seen(team)} {
    puts stderr "Native credential validation failed."
    exit 1
}
'''


@contextlib.contextmanager
def signing_credentials():
    names = (
        "APPLE_CERTIFICATE_P12_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY",
        "APPLE_ID", "APPLE_TEAM_ID", "APPLE_APP_SPECIFIC_PASSWORD",
    )
    credentials = {name: required(name) for name in names}
    original = shlex.split(run(["/usr/bin/security", "list-keychains", "-d", "user"]).stdout)
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        keychain = root / "signing.keychain-db"
        p12 = root / "identity.p12"
        pem = root / "identity.pem"
        p12.write_bytes(base64.b64decode("".join(credentials["APPLE_CERTIFICATE_P12_BASE64"].split()), validate=True))
        try:
            run(["/usr/bin/openssl", "pkcs12", "-in", p12, "-passin", "stdin", "-nodes", "-out", pem],
                input=credentials["APPLE_CERTIFICATE_PASSWORD"] + "\n")
            run(["/usr/bin/security", "create-keychain", "-p", "", keychain])
            run(["/usr/bin/security", "set-keychain-settings", "-lut", "3600", keychain])
            run(["/usr/bin/security", "unlock-keychain", "-p", "", keychain])
            run(["/usr/bin/security", "list-keychains", "-d", "user", "-s", keychain, *original])
            run(["/usr/bin/security", "import", pem, "-f", "pemseq", "-k", keychain, "-T", "/usr/bin/codesign"])
            run(["/usr/bin/security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", "", keychain])
            p12.unlink()
            pem.unlink()
            identities = re.findall(
                r'^\s*\d+\)\s+([A-Fa-f0-9]{40})\s+"(.*)"\s*$',
                run(["/usr/bin/security", "find-identity", "-v", "-p", "codesigning", keychain]).stdout, re.M,
            )
            matches = [fingerprint for fingerprint, name in identities if name == credentials["APPLE_SIGNING_IDENTITY"]]
            if len(matches) != 1:
                raise ReleaseError("No unique, valid imported identity matches APPLE_SIGNING_IDENTITY.")
            run(["/usr/bin/expect"], input=NOTARY_AUTH, env={**os.environ, "CITO_KEYCHAIN": str(keychain)})
            yield keychain, matches[0]
        finally:
            run(["/usr/bin/security", "list-keychains", "-d", "user", "-s", *original])
            if keychain.exists():
                run(["/usr/bin/security", "delete-keychain", keychain])


def write_checksum(archive):
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_text(f"{digest}  {archive.name}\n")
    return digest


def verify_archive(archive):
    expected = archive.with_suffix(archive.suffix + ".sha256").read_text().split()
    with archive.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if expected != [actual, archive.name]:
        raise ReleaseError("Release checksum does not match its immutable ZIP.")
    with tempfile.TemporaryDirectory() as directory:
        run(["/usr/bin/ditto", "-x", "-k", archive, directory])
        app = Path(directory) / "Cito.app"
        verify_bundle(app, version())
        run(["/usr/bin/xcrun", "stapler", "validate", app])


def notarize(app, archive):
    if archive.exists():
        raise ReleaseError("Refusing to overwrite an existing release archive.")
    verify_bundle(app, version())
    archive.parent.mkdir(parents=True, exist_ok=True)
    with signing_credentials() as (keychain, identity):
        sign_bundle(app, identity, keychain=keychain)
        details = run(["/usr/bin/codesign", "--display", "--verbose=4", app]).stderr
        team = re.search(r"^TeamIdentifier=(.+)$", details, re.M)
        if not team or team.group(1) != required("APPLE_TEAM_ID"):
            raise ReleaseError("Signed bundle Team ID does not match APPLE_TEAM_ID.")
        with tempfile.TemporaryDirectory() as directory:
            submission_zip = Path(directory) / f"Cito-v{version()}-notarize.zip"
            run(["/usr/bin/ditto", "-c", "-k", "--keepParent", app, submission_zip])
            result = run([
                "/usr/bin/xcrun", "notarytool", "submit", submission_zip,
                "--keychain-profile", "cito-release", "--keychain", keychain,
                "--wait", "--timeout", "20m", "--output-format", "json",
            ], check=False, timeout=1260)
            try:
                report = json.loads(result.stdout)
            except json.JSONDecodeError:
                report = {}
            if re.fullmatch(r"[0-9a-fA-F-]{36}", report.get("id", "")):
                print(f"Apple notarization submission ID: {report['id']}")
            if result.returncode or report.get("status") != "Accepted":
                raise ReleaseError("Notarization did not complete as Accepted. Reconcile Apple's submission history/log before retrying; no release was published.")
        run(["/usr/bin/xcrun", "stapler", "staple", app])
        run(["/usr/bin/xcrun", "stapler", "validate", app])
        verify_bundle(app, version())
        run(["/usr/bin/ditto", "-c", "-k", "--keepParent", app, archive])
    write_checksum(archive)
    print("Developer ID signing, notarization, staple, and final archive verification passed.")


def repository():
    value = required("GITHUB_REPOSITORY")
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", value):
        raise ReleaseError("Invalid GitHub repository target.")
    return value


def gh(args, *, missing_ok=False):
    result = run(["gh", *args], check=False)
    if result.returncode:
        if missing_ok and ("release not found" in result.stderr.lower() or "HTTP 404" in result.stderr):
            return None
        raise ReleaseError("GitHub operation failed; reconcile the target state before retrying.")
    return json.loads(result.stdout) if result.stdout.strip() else None


def release_info(tag):
    return gh(["release", "view", tag, "--repo", repository(), "--json", "assets,isDraft,tagName"], missing_ok=True)


def candidate(sha, destination):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ReleaseError("Candidate lookup requires an exact source commit.")
    runs = gh(["run", "list", "--repo", repository(), "--workflow", "ci.yml", "--branch", "main", "--event", "push", "--commit", sha,
               "--limit", "20", "--json", "databaseId,status,conclusion,headSha,headBranch"])
    runs = [item for item in runs if item["headSha"] == sha and item["headBranch"] == "main"]
    if not runs:
        output("reused", "false")
        return
    current = runs[0]
    if current["status"] != "completed":
        run(["gh", "run", "watch", str(current["databaseId"]), "--repo", repository(), "--interval", "10", "--exit-status"], check=False, timeout=1200)
        current = gh(["run", "view", str(current["databaseId"]), "--repo", repository(), "--json", "status,conclusion"])
    if current["conclusion"] == "cancelled":
        output("reused", "false")
        return
    if current["conclusion"] != "success":
        raise ReleaseError("CI for the selected source commit did not pass.")
    destination.parent.mkdir(parents=True, exist_ok=True)
    result = run(["gh", "run", "download", str(runs[0]["databaseId"]), "--repo", repository(), "--name", f"cito-candidate-{sha}", "--dir", destination.parent], check=False)
    output("reused", str(result.returncode == 0 and destination.is_file()).lower())


def validate_tag(tag):
    if tag != f"v{check_metadata()}":
        raise ReleaseError("The selected tag does not match the canonical release version.")
    if run(["git", "cat-file", "-t", f"refs/tags/{tag}"], cwd=ROOT).stdout.strip() != "tag":
        raise ReleaseError("Release tags must be annotated.")
    sha = run(["git", "rev-parse", f"refs/tags/{tag}^{{commit}}"], cwd=ROOT).stdout.strip()
    run(["git", "merge-base", "--is-ancestor", sha, "origin/main"], cwd=ROOT)
    if run(["git", "rev-parse", "HEAD"], cwd=ROOT).stdout.strip() != sha:
        raise ReleaseError("Checkout does not match the selected release tag.")
    output("sha", sha)
    output("version", version())


def existing_assets(tag, directory):
    info = release_info(tag)
    if not info:
        output("reused", "false")
        return
    names = {asset["name"] for asset in info["assets"]}
    if ARCHIVE not in names:
        if not info["isDraft"] or names:
            raise ReleaseError("Existing release assets are incomplete. Reconcile them before retrying.")
        output("reused", "false")
        return
    directory.mkdir(parents=True, exist_ok=True)
    run(["gh", "release", "download", tag, "--repo", repository(), "--pattern", ARCHIVE, "--dir", directory])
    archive = directory / ARCHIVE
    checksum_file = directory / (ARCHIVE + ".sha256")
    if checksum_file.name in names:
        run(["gh", "release", "download", tag, "--repo", repository(), "--pattern", checksum_file.name, "--dir", directory])
    elif info["isDraft"]:
        write_checksum(archive)
    else:
        raise ReleaseError("Published release is missing its checksum; refusing to mutate it.")
    verify_archive(archive)
    output("reused", "true")


def publish(tag, directory):
    archive = directory / ARCHIVE
    checksum = directory / (ARCHIVE + ".sha256")
    verify_archive(archive)
    info = release_info(tag)
    if not info:
        run(["gh", "release", "create", tag, archive, checksum, "--repo", repository(), "--draft", "--verify-tag", "--title", f"Cito {tag}", "--generate-notes"])
        info = release_info(tag)
        if info is None:
            raise ReleaseError("Created release could not be read back. Reconcile it before retrying.")
    elif info["assets"]:
        with tempfile.TemporaryDirectory() as existing:
            existing_assets(tag, Path(existing))
            if (Path(existing) / checksum.name).read_text() != checksum.read_text():
                raise ReleaseError("Existing release payload differs; immutable assets will not be replaced.")
    if info["isDraft"]:
        names = {asset["name"] for asset in info["assets"]}
        for asset in (archive, checksum):
            if asset.name not in names:
                run(["gh", "release", "upload", tag, asset, "--repo", repository()])
    if info["isDraft"]:
        pages = gh(["api", "--paginate", "--slurp", f"repos/{repository()}/releases?per_page=100"])
        newer = False
        for release in (item for page in pages for item in page):
            if release["draft"] or release["prerelease"]:
                continue
            try:
                newer |= version_tuple(release["tag_name"].removeprefix("v")) > version_tuple(version())
            except ReleaseError:
                continue
        run(["gh", "release", "edit", tag, "--repo", repository(), "--draft=false", f"--latest={str(not newer).lower()}"])
    final = release_info(tag)
    if final is None or final["isDraft"] or not {ARCHIVE, ARCHIVE + ".sha256"}.issubset({asset["name"] for asset in final["assets"]}):
        raise ReleaseError("Published release could not be verified. Reconcile it before retrying.")
    print(f"Release {tag} is published; existing assets were not overwritten.")


def smoke_cask(cask, tap, expected):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        env = {**isolated_env(home), "HOMEBREW_NO_AUTO_UPDATE": "1", "HOMEBREW_NO_INSTALL_CLEANUP": "1", "HOMEBREW_NO_ANALYTICS": "1"}
        prefix = Path(run(["brew", "--prefix"], env=env).stdout.strip())
        links = [prefix / "bin" / name for name in ("cito", "cito-mcp")]
        if any(os.path.lexists(path) for path in links):
            raise ReleaseError("Install test requires a clean runner with no existing Cito command links.")
        installed = run(["brew", "list", "--cask", "--versions", "cito"], env=env, check=False)
        if installed.returncode == 0 and installed.stdout.strip():
            raise ReleaseError("Install test requires a runner without an existing Cito cask.")
        run(["brew", "tap", tap], env=env)
        tap_root = Path(run(["brew", "--repo", tap], env=env).stdout.strip())
        destination = tap_root / "Casks/cito.rb"
        original = destination.read_bytes() if destination.exists() else None
        legacy = tap_root / "Formula/cito.rb"
        legacy_contents = legacy.read_bytes() if legacy.exists() else None
        app = root / "Applications/Cito.app"
        marker = home / ".cito/keep-on-uninstall"
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text("preserve\n")
        try:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(cask.read_bytes())
            legacy.unlink(missing_ok=True)
            run(["brew", "style", "--cask", f"{tap}/cito"], env=env)
            run(["brew", "install", f"{tap}/cito", f"--appdir={app.parent}"], env=env)
            if not app.is_dir():
                raise ReleaseError("Cask did not install the App into its configured appdir.")
            for link in links:
                if not link.is_symlink() or link.resolve() != app / "Contents/MacOS/Cito":
                    raise ReleaseError("Cask command link does not point into the installed App.")
                if run([link, "--version"], env=env).stdout.strip() != f"cito v{expected}":
                    raise ReleaseError("Installed command reported the wrong version.")
            run(["brew", "uninstall", f"{tap}/cito"], env=env)
            if os.path.lexists(app) or any(os.path.lexists(link) for link in links):
                raise ReleaseError("Ordinary cask uninstall left the App or command links behind.")
            if marker.read_text() != "preserve\n":
                raise ReleaseError("Ordinary cask uninstall changed user data.")
        finally:
            run(["brew", "uninstall", "--cask", f"{tap}/cito"], env=env, check=False)
            for link in links:
                if link.is_symlink() and link.readlink() == app / "Contents/MacOS/Cito":
                    link.unlink()
            if original is None:
                destination.unlink(missing_ok=True)
            else:
                destination.write_bytes(original)
            if legacy_contents is not None:
                legacy.write_bytes(legacy_contents)
    print("Cask install, both CLI links, uninstall, and user-data preservation verified.")


@contextlib.contextmanager
def tap_ssh(root):
    key = required("HOMEBREW_TAP_SSH_KEY")
    keys = gh(["api", "meta"])["ssh_keys"]
    known_hosts = root / "known_hosts"
    known_hosts.write_text("".join(f"github.com {value}\n" for value in keys))
    agent = run(["ssh-agent", "-s"]).stdout
    env = {**os.environ, **dict(re.findall(r"(SSH_AUTH_SOCK|SSH_AGENT_PID)=([^;]+);", agent))}
    env["GIT_SSH_COMMAND"] = f"ssh -o StrictHostKeyChecking=yes -o UserKnownHostsFile={shlex.quote(str(known_hosts))}"
    try:
        run(["ssh-add", "-"], input=key + "\n", env=env)
        yield env
    finally:
        run(["ssh-agent", "-k"], env=env)


def tap_allows_update(current, desired, expected):
    def fields(text):
        versions = re.findall(r'^\s*version "([^"]+)"', text, re.M)
        hashes = re.findall(r'^\s*sha256 "([0-9a-f]{64})"', text, re.M)
        if len(versions) != 1 or len(hashes) != 1:
            raise ReleaseError("Cask must have one concrete version and archive checksum.")
        version_tuple(versions[0])
        return versions[0], hashes[0]

    desired_version, desired_hash = fields(desired)
    if desired_version != expected:
        raise ReleaseError("Generated Cask does not match the selected release version.")
    if current is None:
        return True
    current_version, current_hash = fields(current)
    if version_tuple(current_version) > version_tuple(expected):
        return False
    if current_version == expected and current_hash != desired_hash:
        raise ReleaseError("Refusing to change the archive checksum of an existing tap version.")
    return True


def update_tap(tag, cask):
    target = required("HOMEBREW_TAP_REPOSITORY")
    if not re.fullmatch(r"[\w.-]+/homebrew-[\w.-]+", target):
        raise ReleaseError("Tap target must be an explicit GitHub owner/homebrew-name repository.")
    owner, name = target.split("/")
    tap = f"{owner}/{name.removeprefix('homebrew-')}"
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        clone = root / "tap"
        run(["git", "clone", "--depth", "1", "--branch", "main", f"https://github.com/{target}.git", clone])
        destination = clone / "Casks/cito.rb"
        current = destination.read_text() if destination.exists() else None
        if not tap_allows_update(current, cask.read_text(), version()):
            print("Tap already contains a newer release; refusing a downgrade.")
            return
        smoke_cask(cask, tap, version())
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(cask.read_bytes())
        (clone / "Formula/cito.rb").unlink(missing_ok=True)
        run(["git", "add", "-A"], cwd=clone)
        if not run(["git", "diff", "--cached", "--name-only"], cwd=clone).stdout.strip():
            print("Tap already matches the verified release.")
            return
        run(["git", "config", "user.name", "github-actions[bot]"], cwd=clone)
        run(["git", "config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com"], cwd=clone)
        run(["git", "-c", "commit.gpgsign=false", "commit", "-m", f"Update Cito to {tag}"], cwd=clone)
        with tap_ssh(root) as env:
            run(["git", "push", f"git@github.com:{target}.git", "HEAD:refs/heads/main"], cwd=clone, env=env)
        expected = run(["git", "rev-parse", "HEAD"], cwd=clone).stdout.strip()
        observed = run(["git", "ls-remote", f"https://github.com/{target}.git", "refs/heads/main"]).stdout.split()
        if not observed or observed[0] != expected:
            raise ReleaseError("Tap head changed after push. Reconcile its current contents before retrying.")
        print(f"Verified Cask pushed to {target} main.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
    command = commands.add_parser("bundle")
    command.add_argument("destination", type=Path)
    command = commands.add_parser("render-cask")
    command.add_argument("--sha256", required=True)
    command.add_argument("--output", required=True, type=Path)
    command = commands.add_parser("validate-tag")
    command.add_argument("tag")
    command = commands.add_parser("candidate")
    command.add_argument("sha")
    command.add_argument("destination", type=Path)
    command = commands.add_parser("assets")
    command.add_argument("tag")
    command.add_argument("directory", type=Path)
    command = commands.add_parser("sign")
    command.add_argument("app", type=Path)
    command.add_argument("archive", type=Path)
    command = commands.add_parser("publish")
    command.add_argument("tag")
    command.add_argument("directory", type=Path)
    command = commands.add_parser("tap")
    command.add_argument("tag")
    command.add_argument("cask", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        if args.command in ("assets", "publish", "tap") and args.tag != f"v{version()}":
            raise ReleaseError("Selected release does not match the canonical version.")
        if args.command == "check":
            print(check_metadata())
        elif args.command == "bundle":
            bundle(args.destination.resolve())
        elif args.command == "render-cask":
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(render_cask((ROOT / "packaging/homebrew/cito.rb").read_text(), version(), args.sha256))
        elif args.command == "validate-tag":
            validate_tag(args.tag)
        elif args.command == "candidate":
            candidate(args.sha, args.destination.resolve())
        elif args.command == "assets":
            existing_assets(args.tag, args.directory.resolve())
        elif args.command == "sign":
            notarize(args.app.resolve(), args.archive.resolve())
        elif args.command == "publish":
            publish(args.tag, args.directory.resolve())
        elif args.command == "tap":
            update_tap(args.tag, args.cask.resolve())
    except (ReleaseError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        if isinstance(error, (ReleaseError, subprocess.TimeoutExpired)):
            message = str(error) if isinstance(error, ReleaseError) else "Native command timed out; reconcile any notarization submission before retrying."
        else:
            message = "Release input or native tool operation failed. No credential values are logged."
        print(f"ERROR: {message}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
