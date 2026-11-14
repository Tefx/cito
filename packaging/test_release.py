"""Behavior tests for version, Cask, artifact, and native signing contracts."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("cito_release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class MetadataTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        crate = self.root / "crates/cito-native/Cargo.toml"
        crate.parent.mkdir(parents=True)
        crate.write_text('[package]\nname = "cito-native"\nversion = "1.2.3"\n')
        (self.root / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "cito-native"\nversion = "1.2.3"\n')
        for name in release.VERSION_FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            value = {"version": "1.2.3"}
            if name.endswith("marketplace.json"):
                value["plugins"] = [{"version": "1.2.3"}]
            path.write_text(json.dumps(value))
        (self.root / "site").mkdir()
        (self.root / "site/index.html").write_text('<meta name="application-version" content="1.2.3" />')

    def test_all_metadata_matches_canonical_crate(self):
        self.assertEqual(release.check_metadata(self.root), "1.2.3")

    def test_lock_plugin_marketplace_and_website_drift_are_rejected(self):
        for relative in ("Cargo.lock", release.VERSION_FILES[2], release.VERSION_FILES[0], "site/index.html"):
            with self.subTest(path=relative):
                path = self.root / relative
                original = path.read_text()
                path.write_text(original.replace("1.2.3", "1.2.4"))
                with self.assertRaises(release.ReleaseError):
                    release.check_metadata(self.root)
                path.write_text(original)

    def test_nested_marketplace_version_is_checked(self):
        path = self.root / release.VERSION_FILES[0]
        data = json.loads(path.read_text())
        data["plugins"][0]["version"] = "1.2.4"
        path.write_text(json.dumps(data))
        with self.assertRaises(release.ReleaseError):
            release.check_metadata(self.root)

    def test_versions_compare_numerically_and_reject_unsupported_tags(self):
        self.assertGreater(release.version_tuple("0.10.0"), release.version_tuple("0.9.99"))
        for value in ("01.2.3", "1.2", "v1.2.3", "1.2.3-rc.1", '1.2.3"; exit'):
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.version_tuple(value)


RUBY_CASK = r'''
require "json"
class Probe
  attr_reader :data
  def initialize
    @data = { "binaries" => [] }
  end
  def version(value = nil)
    @data["version"] = value if value
    @data["version"]
  end
  def appdir
    ENV.fetch("TEST_APPDIR")
  end
  def binary(source, target:)
    @data["binaries"] << [source, target]
  end
  def method_missing(name, *args, **kwargs)
    @data[name.to_s] = kwargs.empty? ? args : kwargs
  end
end
def cask(name, &block)
  probe = Probe.new
  probe.instance_eval(&block)
  puts JSON.generate(probe.data.merge("token" => name))
end
load ARGV.fetch(0)
'''


class ArtifactTests(unittest.TestCase):
    def test_renderer_binds_version_checksum_url_and_both_appdir_links(self):
        template = (release.ROOT / "packaging/homebrew/cito.rb").read_text()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cito.rb"
            for version, checksum in (("1.2.3", "a" * 64), ("2.0.1", "b" * 64)):
                path.write_text(release.render_cask(template, version, checksum))
                for appdir in ("/Applications", "/tmp/custom-applications"):
                    result = subprocess.run(
                        ["/usr/bin/ruby", "-e", RUBY_CASK, str(path)],
                        env={**os.environ, "TEST_APPDIR": appdir}, text=True, capture_output=True, check=True,
                    )
                    data = json.loads(result.stdout)
                    self.assertEqual(data["version"], version)
                    self.assertEqual(data["sha256"], [checksum])
                    self.assertEqual(data["url"], [f"https://github.com/Tefx/cito/releases/download/v{version}/{release.ARCHIVE}"])
                    self.assertEqual(data["app"], ["Cito.app"])
                    self.assertEqual(data["binaries"], [
                        [f"{appdir}/Cito.app/Contents/MacOS/Cito", "cito"],
                        [f"{appdir}/Cito.app/Contents/MacOS/Cito", "cito-mcp"],
                    ])
                    self.assertEqual(data["uninstall"], {"quit": "one.tefx.cito"})
                    self.assertIn("~/.cito", data["zap"]["trash"])

    def test_cask_smoke_accepts_symlinked_temporary_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            work = root / "work"
            work.mkdir()
            alias = root / "temporary-alias"
            alias.symlink_to(work, target_is_directory=True)
            prefix = root / "prefix"
            (prefix / "bin").mkdir(parents=True)
            tap = root / "tap"
            cask = root / "cito.rb"
            cask.write_text("cask fixture\n")
            links = [prefix / "bin" / name for name in ("cito", "cito-mcp")]
            app = work / "Applications/Cito.app"
            checked = []

            def brew(command, **kwargs):
                if command[:2] == ["brew", "--prefix"]:
                    output = str(prefix)
                elif command[:2] == ["brew", "--repo"]:
                    output = str(tap)
                elif command[:2] == ["brew", "install"]:
                    launcher = app / "Contents/MacOS/Cito"
                    launcher.parent.mkdir(parents=True)
                    launcher.write_text("launcher fixture\n")
                    for link in links:
                        link.symlink_to(alias / "Applications/Cito.app/Contents/MacOS/Cito")
                    output = ""
                elif command[:2] == ["brew", "uninstall"]:
                    for link in links:
                        link.unlink(missing_ok=True)
                    shutil.rmtree(app, ignore_errors=True)
                    output = ""
                elif command[0] in links:
                    checked.append(command[0].name)
                    output = "cito v1.2.3\n"
                else:
                    output = ""
                return subprocess.CompletedProcess(command, 0, output, "")

            with patch.object(release.tempfile, "TemporaryDirectory", return_value=contextlib.nullcontext(str(alias))), patch.object(release, "run", side_effect=brew), contextlib.redirect_stdout(io.StringIO()):
                release.smoke_cask(cask, "acme/tap", "1.2.3")
            self.assertEqual(checked, ["cito", "cito-mcp"])
            self.assertEqual((work / "home/.cito/keep-on-uninstall").read_text(), "preserve\n")

    def test_renderer_rejects_bad_checksum_or_ambiguous_template(self):
        template = (release.ROOT / "packaging/homebrew/cito.rb").read_text()
        for checksum in ("a" * 63, "A" * 64, 'a"; exit'):
            with self.subTest(checksum=checksum), self.assertRaises(release.ReleaseError):
                release.render_cask(template, "1.2.3", checksum)
        for broken in (template.replace("__VERSION__", "1.2.3"), template + "__SHA256__"):
            with self.assertRaises(release.ReleaseError):
                release.render_cask(broken, "1.2.3", "a" * 64)

    def test_tap_guard_prevents_downgrade_payload_replacement_and_wrong_version(self):
        def cask(version, checksum):
            return f'version "{version}"\nsha256 "{checksum}"\n'

        desired = cask("0.10.0", "a" * 64)
        self.assertTrue(release.tap_allows_update(cask("0.9.9", "b" * 64), desired, "0.10.0"))
        self.assertFalse(release.tap_allows_update(cask("0.11.0", "b" * 64), desired, "0.10.0"))
        self.assertTrue(release.tap_allows_update(desired, desired, "0.10.0"))
        with self.assertRaisesRegex(release.ReleaseError, "existing tap version"):
            release.tap_allows_update(cask("0.10.0", "b" * 64), desired, "0.10.0")
        with self.assertRaisesRegex(release.ReleaseError, "selected release version"):
            release.tap_allows_update(None, desired, "0.10.1")
        with self.assertRaises(release.ReleaseError):
            release.tap_allows_update(None, 'version "0.10.0"', "0.10.0")

    def test_archive_payload_change_is_rejected_before_extraction(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / release.ARCHIVE
            archive.write_bytes(b"original")
            release.write_checksum(archive)
            archive.write_bytes(b"changed")
            with patch.object(release, "run") as native:
                with self.assertRaisesRegex(release.ReleaseError, "checksum"):
                    release.verify_archive(archive)
                native.assert_not_called()

    def test_checksum_describes_final_archive_bytes_and_filename(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / release.ARCHIVE
            archive.write_bytes(b"final signed and stapled archive fixture")
            expected = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertEqual(release.write_checksum(archive), expected)
            self.assertEqual(archive.with_suffix(".zip.sha256").read_text(), f"{expected}  {release.ARCHIVE}\n")

    def test_child_failure_does_not_expose_native_diagnostics(self):
        with self.assertRaises(release.ReleaseError) as error:
            release.run([sys.executable, "-c", 'import sys; print("sensitive-fixture", file=sys.stderr); sys.exit(7)'])
        self.assertIn("7", str(error.exception))
        self.assertNotIn("sensitive-fixture", str(error.exception))

    def test_bundle_requires_direct_ui_helper_instead_of_a_plugin(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "Cito.app"
            resources = app / "Contents/Resources"
            (resources / "plugins").mkdir(parents=True)
            (app / "Contents/Helpers").mkdir()
            with (app / "Contents/Info.plist").open("wb") as stream:
                plistlib.dump({"CFBundleShortVersionString": "1.2.3", "CFBundleVersion": "1.2.3"}, stream)
            helper = app / "Contents/Helpers/cito-ui"
            for path in (helper, app / "Contents/Helpers/cito-native", resources / "AppIcon.icns"):
                path.write_bytes(b"component fixture")

            def native(command, **kwargs):
                output = "cito v1.2.3\n" if command[-1] == "--version" else "arm64\n"
                return subprocess.CompletedProcess(command, 0, output, "")

            with patch.object(release, "run", side_effect=native) as commands:
                release.verify_bundle(app, "1.2.3")
                self.assertTrue(any(call.args[0][-1] == helper for call in commands.call_args_list))
            helper.unlink()
            retired = resources / "plugins/human/bin/cito-ui"
            retired.parent.mkdir(parents=True)
            retired.write_bytes(b"retired component fixture")
            with patch.object(release, "run") as commands:
                with self.assertRaisesRegex(release.ReleaseError, "Contents/Helpers/cito-ui"):
                    release.verify_bundle(app, "1.2.3")
                commands.assert_not_called()

    def test_app_and_brew_environment_does_not_inherit_credentials(self):
        with patch.dict(os.environ, {"APPLE_CERTIFICATE_PASSWORD": "fixture", "GH_TOKEN": "fixture", "HOMEBREW_TAP_SSH_KEY": "fixture", "SSH_AUTH_SOCK": "fixture"}):
            env = release.isolated_env(Path("/tmp/isolated-home"))
        self.assertFalse({"APPLE_CERTIFICATE_PASSWORD", "GH_TOKEN", "HOMEBREW_TAP_SSH_KEY", "SSH_AUTH_SOCK"} & env.keys())
        self.assertEqual(env["CITO_UI_HEADLESS"], "1")

    def test_candidate_requires_exact_main_commit_and_rejects_failed_ci(self):
        sha = "a" * 40
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "candidate.zip"
            cases = [
                ([{"headSha": "b" * 40, "headBranch": "main"}], False),
                ([{"headSha": sha, "headBranch": "feature"}], False),
                ([{"headSha": sha, "headBranch": "main", "status": "completed", "conclusion": "failure"}], True),
            ]
            for runs, fails in cases:
                with self.subTest(runs=runs), patch.dict(os.environ, {"GITHUB_REPOSITORY": "acme/service"}), patch.object(release, "gh", return_value=runs), patch.object(release, "run") as download:
                    with contextlib.redirect_stdout(io.StringIO()) as stdout:
                        if fails:
                            with self.assertRaisesRegex(release.ReleaseError, "selected source commit did not pass"):
                                release.candidate(sha, destination)
                        else:
                            release.candidate(sha, destination)
                            self.assertIn("reused=false", stdout.getvalue())
                    download.assert_not_called()

    @unittest.skipUnless(sys.platform == "darwin", "Native codesign requires macOS")
    def test_real_adhoc_signing_seals_helpers_before_outer_app(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "Probe.app"
            executable = app / "Contents/MacOS/probe"
            helper = app / "Contents/Helpers/helper"
            executable.parent.mkdir(parents=True)
            helper.parent.mkdir(parents=True)
            with (app / "Contents/Info.plist").open("wb") as stream:
                plistlib.dump({"CFBundleIdentifier": "com.example.signing-probe", "CFBundleExecutable": "probe", "CFBundlePackageType": "APPL", "CFBundleVersion": "1.0.0"}, stream)
            subprocess.run(["/usr/bin/clang", "-x", "c", "-", "-o", executable], input="int main(void) { return 0; }\n", text=True, check=True, capture_output=True)
            shutil.copyfile(executable, helper)
            helper.chmod(0o755)
            order = release.signing_order(app)
            self.assertEqual(order[-1], app)
            self.assertIn(helper, order[:-1])
            release.sign_bundle(app, "-", timestamp=False)
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app], check=True, capture_output=True)
            # Cito's real App uses a shell entrypoint with a native helper.
            executable.write_text('#!/bin/sh\nexec "$(dirname "$0")/../Helpers/helper" "$@"\n')
            executable.chmod(0o755)
            release.sign_bundle(app, "-", timestamp=False)
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app], check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
