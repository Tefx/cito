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
import zipfile
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("cito_release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)

HOST_FILES = (
    "claude/.claude-plugin/plugin.json", "claude/.mcp.json", "claude/hooks/hooks.json",
    "codex/.codex-plugin/plugin.json", "codex/.mcp.json",
)
HOOK = "Resources/plugins/claude/bin/cito-claude-hook"


def bundle_fixture(app, version, plugin_ids, *, nested=True):
    contents = app / "Contents"
    quit_targets = cask_data(release.ROOT / "packaging/homebrew/cito.rb")["uninstall"]["quit"]
    outer_id, *nested_ids = [quit_targets] if isinstance(quit_targets, str) else quit_targets
    info = {"CFBundleIdentifier": outer_id, "CFBundleShortVersionString": version, "CFBundleVersion": version}
    ui = "Helpers/cito-ui"
    files = {
        "Info.plist": plistlib.dumps(info),
        "MacOS/Cito": b"launcher", "Helpers/cito-native": b"helper",
        "Resources/AppIcon.icns": b"icon",
        "Resources/plugins/pi/package.json": json.dumps({"version": version, "pi": {"extensions": ["./cito-bridge.ts"]}}).encode(),
        "Resources/plugins/pi/cito-bridge.ts": b"export {};",
        HOOK: b"hook",
        **{f"Resources/plugins/{name}": b"{}" for name in HOST_FILES},
    }
    if nested:
        nested_id, = nested_ids
        ui = "Helpers/Cito.app/Contents/MacOS/cito-ui"
        files["Helpers/Cito.app/Contents/Info.plist"] = plistlib.dumps({
            **info, "CFBundleIdentifier": nested_id, "CFBundleExecutable": "cito-ui",
        })
    files[ui] = b"helper"
    for plugin in plugin_ids:
        files[f"Resources/plugins/{plugin}/plugin.json"] = json.dumps({"version": version, "command": ["./driver"]}).encode()
        files[f"Resources/plugins/{plugin}/driver"] = b"plugin driver"
    for name, data in files.items():
        path = contents / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        executable = name in ("MacOS/Cito", "Helpers/cito-native", ui, HOOK) or name.endswith("/driver")
        path.chmod(0o755 if executable else 0o644)
    for path in (app, *app.rglob("*")):
        if path.is_dir():
            path.chmod(0o755)
    return contents / ui


class BundleVerificationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.app = Path(self.directory.name) / "Cito.app"
        self.plugin_ids = [path.parent.name for path in (release.ROOT / "plugins").glob("*/plugin.json")]
        bundle_fixture(self.app, "1.2.3", self.plugin_ids, nested=False)
        self.plugin = self.app / "Contents/Resources/plugins/claude"
        self.driver = self.plugin / "driver"

    def test_pi_extension_requires_a_readable_package_and_declared_file(self):
        pi = self.plugin.parent / "pi"
        with patch.object(release, "run", side_effect=self.command):
            for name in ("package.json", "cito-bridge.ts"):
                path = pi / name
                original = path.read_bytes()
                path.unlink()
                with self.subTest(name=name, missing=True), self.assertRaisesRegex(release.ReleaseError, name):
                    release.verify_bundle(self.app, "1.2.3")
                path.write_bytes(original)
                path.chmod(0o600)
                with self.subTest(name=name, private=True), self.assertRaisesRegex(release.ReleaseError, name):
                    release.verify_bundle(self.app, "1.2.3")
                path.chmod(0o644)
            package = pi / "package.json"
            package.write_text('{"version":"1.2.3","pi":{"extensions":["./other.ts"]}}')
            with self.assertRaisesRegex(release.ReleaseError, "must declare"):
                release.verify_bundle(self.app, "1.2.3")
            package.write_text('{"version":"1.2.2","pi":{"extensions":["./cito-bridge.ts"]}}')
            with self.assertRaisesRegex(release.ReleaseError, "Packaged Pi extension version"):
                release.verify_bundle(self.app, "1.2.3")

    def test_host_integration_files_and_executable_hook_are_required(self):
        with patch.object(release, "run", side_effect=self.command) as native:
            for name in (HOOK, *(f"Resources/plugins/{name}" for name in HOST_FILES)):
                path = self.app / "Contents" / name
                absent = path.with_suffix(".absent")
                path.rename(absent)
                with self.subTest(name=name), self.assertRaisesRegex(release.ReleaseError, "missing a required release component"):
                    release.verify_bundle(self.app, "1.2.3")
                absent.rename(path)
            hook = self.app / "Contents" / HOOK
            hook.chmod(0o655)
            with self.assertRaisesRegex(release.ReleaseError, "public permissions"):
                release.verify_bundle(self.app, "1.2.3")
            native.assert_not_called()
            hook.chmod(0o755)
            release.verify_bundle(self.app, "1.2.3")
            native.assert_any_call(["/usr/bin/lipo", "-archs", hook])

    def test_cask_matches_outer_and_nested_bundle_identifiers(self):
        nested_app = self.app.parent / "nested" / "Cito.app"
        nested_ui = bundle_fixture(nested_app, "1.2.3", self.plugin_ids)
        outer_id, nested_id = cask_data(release.ROOT / "packaging/homebrew/cito.rb")["uninstall"]["quit"]
        for app, ui in ((self.app, self.app / "Contents/Helpers/cito-ui"), (nested_app, nested_ui)):
            with self.subTest(app=app), patch.object(release, "run", side_effect=self.command) as native:
                release.verify_bundle(app, "1.2.3")
                native.assert_any_call(["/usr/bin/lipo", "-archs", ui])

        flat_plist = self.app / "Contents/Info.plist"
        outer_plist = nested_app / "Contents/Info.plist"
        nested_plist = nested_app / "Contents/Helpers/Cito.app/Contents/Info.plist"
        for app, replacements in (
            (self.app, {flat_plist: "com.example.wrong"}),
            (self.app, {flat_plist: nested_id}),
            (nested_app, {outer_plist: "com.example.wrong"}),
            (nested_app, {nested_plist: "com.example.wrong"}),
            (nested_app, {outer_plist: nested_id, nested_plist: outer_id}),
        ):
            originals = {path: path.read_bytes() for path in replacements}
            for path, identifier in replacements.items():
                info = plistlib.loads(originals[path])
                path.write_bytes(plistlib.dumps({**info, "CFBundleIdentifier": identifier}))
            with self.subTest(replacements=replacements), patch.object(release, "run") as native:
                with self.assertRaisesRegex(release.ReleaseError, "Cask quit target"):
                    release.verify_bundle(app, "1.2.3")
                native.assert_not_called()
            for path, data in originals.items():
                path.write_bytes(data)

    def test_private_directories_and_executables_fail_before_native_checks(self):
        with patch.object(release, "run") as native:
            for path in (self.app, self.plugin, self.driver, self.app / "Contents/Helpers/cito-ui"):
                path.chmod(0o744)
                with self.subTest(path=path), self.assertRaisesRegex(release.ReleaseError, "public permissions"):
                    release.verify_bundle(self.app, "1.2.3")
                path.chmod(0o755)
            native.assert_not_called()

    def test_all_builtin_plugins_must_be_present_at_the_expected_version(self):
        manifest = self.plugin / "plugin.json"
        value = json.loads(manifest.read_text())
        manifest.unlink()
        with patch.object(release, "run", side_effect=self.command):
            with self.assertRaisesRegex(release.ReleaseError, "Built-in plugin missing or version mismatched: claude"):
                release.verify_bundle(self.app, "1.2.3")
            value["version"] = "1.2.2"
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(release.ReleaseError, "Built-in plugin missing or version mismatched: claude"):
                release.verify_bundle(self.app, "1.2.3")

    def command(self, argv, **kwargs):
        output = "arm64" if str(argv[0]).endswith("lipo") else "cito v1.2.3"
        return subprocess.CompletedProcess(argv, 0, output, "")

    def test_missing_and_nonexecutable_manifest_commands_are_rejected(self):
        self.driver.unlink()
        with patch.object(release, "run", side_effect=self.command):
            for exists in (False, True):
                if exists:
                    self.driver.write_text("fixture")
                    self.driver.chmod(0o600)
                with self.subTest(exists=exists), self.assertRaisesRegex(release.ReleaseError, "missing or not executable"):
                    release.verify_bundle(self.app, "1.2.3")

    def test_manifest_command_outside_bin_is_architecture_verified(self):
        self.driver.write_text("fixture")
        self.driver.chmod(0o755)
        with patch.object(release, "run", side_effect=self.command) as run:
            release.verify_bundle(self.app, "1.2.3")
            self.assertTrue(any(call.args[0] == ["/usr/bin/lipo", "-archs", self.driver] for call in run.call_args_list))
        def wrong_arch(argv, **kwargs):
            if argv[-1] == self.driver:
                return subprocess.CompletedProcess(argv, 0, "x86_64", "")
            return self.command(argv, **kwargs)
        with patch.object(release, "run", side_effect=wrong_arch), self.assertRaisesRegex(release.ReleaseError, "arm64"):
            release.verify_bundle(self.app, "1.2.3")


class CleanupTests(unittest.TestCase):
    def test_secondary_failure_does_not_skip_cleanup_or_mask_primary(self):
        observed = []
        def failed():
            observed.append("first")
            raise OSError("sensitive cleanup diagnostic")
        def last():
            observed.append("last")
        with contextlib.redirect_stderr(io.StringIO()) as output:
            with self.assertRaisesRegex(RuntimeError, "primary"):
                try:
                    raise RuntimeError("primary")
                finally:
                    release.cleanup_steps(failed, last)
            self.assertEqual(observed, ["first", "last"])
            self.assertNotIn("sensitive", output.getvalue())
            observed.clear()
            with self.assertRaises(release.ReleaseError):
                release.cleanup_steps(failed, last)
            self.assertEqual(observed, ["first", "last"])


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

    def test_empty_marketplace_cannot_pass_with_matching_top_level_version(self):
        path = self.root / release.VERSION_FILES[0]
        path.write_text(json.dumps({"version": "1.2.3", "plugins": []}))
        with self.assertRaisesRegex(release.ReleaseError, "at least one plugin"):
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


def cask_data(path, appdir="/Applications"):
    result = subprocess.run(
        ["/usr/bin/ruby", "-e", RUBY_CASK, str(path)],
        env={**os.environ, "TEST_APPDIR": appdir}, text=True, capture_output=True, check=True,
    )
    return json.loads(result.stdout)


class ReleaseAssetTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.local = self.root / "local"
        self.local.mkdir()
        self.tag = f"v{release.version()}"
        self.archive = self.local / release.ARCHIVE
        app = self.root / "Cito.app"
        bundle_fixture(app, release.version(), [path.parent.name for path in (release.ROOT / "plugins").glob("*/plugin.json")])
        with zipfile.ZipFile(self.archive, "w") as archive:
            for path in (app, *app.rglob("*")):
                archive.write(path, path.relative_to(self.root))
        release.write_checksum(self.archive)
        self.remote = {
            path.name: path.read_bytes() for path in self.local.iterdir()
        }
        self.output = self.root / "outputs"
        self.output.write_text("sentinel=yes\n")
        self.infos = []
        self.commands = []
        self.reject_staple = False
        self.team_details = "TeamIdentifier=A1B2C3D4E5\n"
        for patcher in (
            patch.dict(os.environ, {"GITHUB_OUTPUT": str(self.output), "GITHUB_REPOSITORY": "acme/service",
                                    "APPLE_TEAM_ID": "A1B2C3D4E5"}),
            patch.object(release, "run", side_effect=self.command),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def info(self, draft=False, names=None):
        if names is None:
            names = self.remote.keys()
        return {"tagName": self.tag, "isDraft": draft, "assets": [{"name": name} for name in names]}

    def command(self, argv, **kwargs):
        self.commands.append(argv)
        stdout = ""
        if argv[:3] == ["gh", "release", "view"]:
            self.assertTrue(self.infos, "Unexpected release query")
            info = self.infos.pop(0)
            if info is None:
                return subprocess.CompletedProcess(argv, 1, "", "release not found")
            stdout = json.dumps(info)
        elif argv[:3] == ["gh", "release", "download"]:
            name = argv[argv.index("--pattern") + 1]
            (Path(argv[argv.index("--dir") + 1]) / name).write_bytes(self.remote[name])
        elif argv[:2] == ["gh", "api"]:
            stdout = "[[]]"
        elif argv[:3] == ["gh", "release", "edit"]:
            # The next view response models the external publish result.
            return subprocess.CompletedProcess(argv, 0, "", "")
        elif argv[:3] == ["gh", "release", "upload"]:
            self.assertEqual(Path(argv[4]).read_bytes(), self.remote[Path(argv[4]).name])
        elif argv[:3] == ["/usr/bin/ditto", "-x", "-k"]:
            with zipfile.ZipFile(argv[3]) as archive:
                archive.extractall(argv[4])
                for entry in archive.infolist():
                    (Path(argv[4]) / entry.filename).chmod(entry.external_attr >> 16)
        elif argv[:2] == ["/usr/bin/codesign", "--verify"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        elif argv[:2] == ["/usr/bin/codesign", "--display"]:
            return subprocess.CompletedProcess(argv, 0, "", self.team_details)
        elif argv[:3] == ["/usr/bin/xcrun", "stapler", "validate"]:
            if self.reject_staple:
                raise release.ReleaseError("Native staple verification failed.")
        elif argv[:2] == ["/usr/bin/lipo", "-archs"]:
            stdout = "arm64"
        elif argv[-1] == "--version":
            stdout = f"cito v{release.version()}"
        else:
            self.fail(f"Unexpected external command: {argv}")
        return subprocess.CompletedProcess(argv, 0, stdout, "")

    def test_assets_cli_reports_reuse_only_for_verified_assets(self):
        for info, reused in ((None, False), (self.info(True, []), False), (self.info(), True),
                             (self.info(True, [release.ARCHIVE]), True)):
            with self.subTest(info=info):
                self.output.write_text("sentinel=yes\n")
                self.infos = [info]
                self.commands.clear()
                destination = self.root / "lookup"
                with patch.object(sys, "argv", ["release.py", "assets", self.tag, str(destination)]), contextlib.redirect_stdout(io.StringIO()) as stdout:
                    previous = os.umask(0o077)
                    try:
                        self.assertEqual(release.main(), 0)
                    finally:
                        os.umask(previous)
                line = f"reused={str(reused).lower()}\n"
                self.assertEqual(stdout.getvalue(), line)
                self.assertEqual(self.output.read_text(), "sentinel=yes\n" + line)
                if reused:
                    self.assertEqual((destination / (release.ARCHIVE + ".sha256")).read_bytes(),
                                     self.remote[release.ARCHIVE + ".sha256"])
                    cli, = (index for index, argv in enumerate(self.commands) if argv[-1] == "--version")
                    for gate in (["/usr/bin/codesign", "--verify"], ["/usr/bin/codesign", "--display"],
                                 ["/usr/bin/xcrun", "stapler", "validate"]):
                        stage, = (index for index, argv in enumerate(self.commands) if argv[:len(gate)] == gate)
                        self.assertLess(stage, cli)

    def test_publish_retry_reuses_assets_without_emitting_workflow_outputs(self):
        for draft, names in ((False, None), (True, None), (True, [release.ARCHIVE])):
            with self.subTest(draft=draft, names=names):
                self.commands.clear()
                info = self.info(draft, names)
                self.infos = [info, info, self.info()]
                with contextlib.redirect_stdout(io.StringIO()) as stdout:
                    release.publish(self.tag, self.local)
                self.assertIn("is published", stdout.getvalue())
                self.assertNotIn("reused=", stdout.getvalue())
                self.assertEqual(self.output.read_text(), "sentinel=yes\n")
                mutations = [argv[2] for argv in self.commands
                             if argv[:2] == ["gh", "release"] and argv[2] in ("create", "upload", "edit")]
                expected = ["edit"] if draft else []
                if names is not None:
                    expected.insert(0, "upload")
                self.assertEqual(mutations, expected)
                self.assertEqual(self.infos, [])

    def test_publish_rejects_disappeared_or_empty_draft_on_requery(self):
        for changed in (None, self.info(True, [])):
            with self.subTest(changed=changed):
                self.commands.clear()
                self.infos = [self.info(True), changed]
                with self.assertRaisesRegex(release.ReleaseError, "changed during verification"):
                    release.publish(self.tag, self.local)
                self.assert_no_mutations()
                self.assertEqual(self.output.read_text(), "sentinel=yes\n")
                self.assertEqual(self.infos, [])

    def assert_no_mutations(self):
        self.assertFalse([argv for argv in self.commands if argv[:2] == ["gh", "release"]
                          and argv[2] in ("create", "upload", "edit")])

    def test_incomplete_and_unverified_assets_are_rejected_by_lookup_and_publish(self):
        checksum = release.ARCHIVE + ".sha256"
        cases = [
            (False, [], False, "incomplete"),
            (True, [checksum], False, "incomplete"),
            (False, [checksum], False, "incomplete"),
            (False, [release.ARCHIVE], False, "missing its checksum"),
            (False, None, True, "checksum"),
            (True, None, True, "checksum"),
        ]
        original = self.remote[checksum]
        for draft, names, corrupt, message in cases:
            for publishing in (False, True):
                with self.subTest(draft=draft, names=names, corrupt=corrupt, publishing=publishing):
                    self.commands.clear()
                    self.remote[checksum] = b"invalid checksum\n" if corrupt else original
                    info = self.info(draft, names)
                    self.infos = [self.info(True), info] if publishing else [info]
                    with self.assertRaisesRegex(release.ReleaseError, message):
                        if publishing:
                            release.publish(self.tag, self.local)
                        else:
                            release.existing_assets(self.tag, self.root / "rejected")
                    self.assert_no_mutations()
                    self.assertEqual(self.output.read_text(), "sentinel=yes\n")
                    self.assertEqual(self.infos, [])

    def test_remote_native_verification_failure_prevents_publish_mutations(self):
        self.reject_staple = True
        for draft in (False, True):
            with self.subTest(draft=draft):
                self.commands.clear()
                self.infos = [self.info(draft)]
                with self.assertRaisesRegex(release.ReleaseError, "staple verification"):
                    release.existing_assets(self.tag, self.root / "invalid-staple")
                self.assert_no_mutations()
                self.assertFalse(any(argv[-1] == "--version" for argv in self.commands))
                self.assertEqual(self.infos, [])
                self.assertEqual(self.output.read_text(), "sentinel=yes\n")

                # Allow the local archive, then reject the downloaded remote copy.
                self.infos = [self.info(draft), self.info(draft)]
                original_command = self.command

                def reject_remote(argv, **kwargs):
                    if argv[:3] == ["/usr/bin/xcrun", "stapler", "validate"] and self.infos:
                        return subprocess.CompletedProcess(argv, 0, "", "")
                    return original_command(argv, **kwargs)

                self.commands.clear()
                with patch.object(release, "run", side_effect=reject_remote):
                    with self.assertRaisesRegex(release.ReleaseError, "staple verification"):
                        release.publish(self.tag, self.local)
                self.assert_no_mutations()
                self.assertEqual(sum(argv[-1] == "--version" for argv in self.commands), 1)
                self.assertEqual(self.infos, [])
                self.assertEqual(self.output.read_text(), "sentinel=yes\n")

    def test_downloaded_archive_requires_expected_team_before_running_cli(self):
        for details, expected, message in (
            ("TeamIdentifier=X1Y2Z3W4V5\n", "A1B2C3D4E5", "does not match"),
            ("TeamIdentifier=not set\n", "A1B2C3D4E5", "does not match"),
            ("TeamIdentifier=not set\n", "not set", "must be a 10-character Apple Team ID"),
            ("", "A1B2C3D4E5", "does not match"),
            ("TeamIdentifier=A1B2C3D4E5\n", "", "Required setting is missing"),
        ):
            with self.subTest(details=details, configured=bool(expected)), patch.dict(os.environ, {"APPLE_TEAM_ID": expected}):
                self.team_details = details
                self.infos = [self.info()]
                self.commands.clear()
                with self.assertRaisesRegex(release.ReleaseError, message):
                    release.existing_assets(self.tag, self.root / "wrong-team")
                self.assertFalse(any(argv[-1] == "--version" for argv in self.commands))
                self.assert_no_mutations()


class ArtifactTests(unittest.TestCase):
    def test_renderer_binds_version_checksum_url_and_both_appdir_links(self):
        source = release.ROOT / "packaging/homebrew/cito.rb"
        template = source.read_text()
        uninstall = cask_data(source)["uninstall"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cito.rb"
            for version, checksum in (("1.2.3", "a" * 64), ("2.0.1", "b" * 64)):
                path.write_text(release.render_cask(template, version, checksum))
                for appdir in ("/Applications", "/tmp/custom-applications"):
                    data = cask_data(path, appdir)
                    self.assertEqual(data["version"], version)
                    self.assertEqual(data["sha256"], [checksum])
                    self.assertEqual(data["url"], [f"https://github.com/Tefx/cito/releases/download/v{version}/{release.ARCHIVE}"])
                    self.assertEqual(data["app"], ["Cito.app"])
                    self.assertEqual(data["binaries"], [
                        [f"{appdir}/Cito.app/Contents/MacOS/Cito", "cito"],
                        [f"{appdir}/Cito.app/Contents/MacOS/Cito", "cito-mcp"],
                    ])
                    self.assertEqual(data["uninstall"], uninstall)
                    self.assertIn("~/.cito", data["zap"]["trash"])

    def test_release_cli_preserves_caller_umask(self):
        with tempfile.TemporaryDirectory() as directory:
            for mask, mode in ((0o022, 0o644), (0o077, 0o600)):
                path = Path(directory) / f"cask-{mask}.rb"
                with self.subTest(mask=mask), patch.object(sys, "argv", ["release.py", "render-cask", "--output", str(path), "--sha256", "a" * 64]):
                    previous = os.umask(mask)
                    try:
                        self.assertEqual(release.main(), 0)
                    finally:
                        observed = os.umask(previous)
                    self.assertEqual(observed, mask)
                    self.assertEqual(path.stat().st_mode & 0o777, mode)

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

    def test_bundle_requires_its_ui_helper_instead_of_a_plugin(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "Cito.app"
            resources = app / "Contents/Resources"
            helper = bundle_fixture(app, "1.2.3", [path.parent.name for path in (release.ROOT / "plugins").glob("*/plugin.json")])
            plugin = resources / "plugins/fixture/bin/channel"
            plugin.parent.mkdir(parents=True)
            plugin.write_bytes(b"executable fixture")
            plugin.chmod(0o755)
            wrong_architecture = False

            def native(command, **kwargs):
                output = "cito v1.2.3\n" if command[-1] == "--version" else "arm64\n"
                if wrong_architecture and command[-1] == plugin:
                    output = "x86_64\n"
                return subprocess.CompletedProcess(command, 0, output, "")

            with patch.object(release, "run", side_effect=native) as commands:
                release.verify_bundle(app, "1.2.3")
                self.assertTrue(any(call.args[0][-1] == helper for call in commands.call_args_list))
                self.assertTrue(any(call.args[0][-1] == plugin for call in commands.call_args_list))
                wrong_architecture = True
                with self.assertRaisesRegex(release.ReleaseError, "arm64"):
                    release.verify_bundle(app, "1.2.3")
            helper.unlink()
            retired = resources / "plugins/human/bin/cito-ui"
            retired.parent.mkdir(parents=True)
            retired.write_bytes(b"retired component fixture")
            with patch.object(release, "run") as commands:
                with self.assertRaisesRegex(release.ReleaseError, "missing a required release component: Contents/Helpers/.*cito-ui"):
                    release.verify_bundle(app, "1.2.3")
                commands.assert_not_called()

    def test_app_and_brew_environment_does_not_inherit_credentials(self):
        with patch.dict(os.environ, {"APPLE_CERTIFICATE_PASSWORD": "fixture", "GH_TOKEN": "fixture", "HOMEBREW_TAP_SSH_KEY": "fixture", "SSH_AUTH_SOCK": "fixture"}):
            env = release.isolated_env(Path("/tmp/isolated-home"))
        self.assertFalse({"APPLE_CERTIFICATE_PASSWORD", "GH_TOKEN", "HOMEBREW_TAP_SSH_KEY", "SSH_AUTH_SOCK"} & env.keys())
        self.assertEqual(env["CITO_UI_HEADLESS"], "1")

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


class CandidateTests(unittest.TestCase):
    sha = "a" * 40
    job = {"name": "Check and build arm64 candidate", "status": "completed", "conclusion": "success"}

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.destination = Path(directory.name) / "candidate.zip"
        self.commands = []
        self.pages = [{"workflow_runs": [self.workflow()]}]
        self.view = {"status": "completed", "conclusion": "success", "jobs": [self.job]}
        self.watch_timeout = False
        self.download_available = True
        for patcher in (
            patch.dict(os.environ, {"GITHUB_REPOSITORY": "acme/service", "GITHUB_OUTPUT": ""}),
            patch.object(release, "run", side_effect=self.command),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def workflow(self, **overrides):
        return {"id": 123, "head_sha": self.sha, "head_branch": "main", "event": "push",
                "status": "completed", "conclusion": "success", **overrides}

    def command(self, argv, **kwargs):
        self.commands.append(argv)
        if argv[:2] == ["gh", "api"]:
            self.assertEqual(argv[2:], ["--paginate", "--slurp",
                f"repos/acme/service/actions/workflows/ci.yml/runs?branch=main&event=push&head_sha={self.sha}&per_page=100"])
            return subprocess.CompletedProcess(argv, 0, json.dumps(self.pages), "")
        if argv[:3] == ["gh", "run", "watch"]:
            self.assertEqual(argv[3], "123")
            self.assertEqual(kwargs["timeout"], 1200)
            if self.watch_timeout:
                raise subprocess.TimeoutExpired(argv, kwargs["timeout"])
            return subprocess.CompletedProcess(argv, 1, "", "Watch did not confirm a pass")
        if argv[:3] == ["gh", "run", "view"]:
            self.assertEqual(argv[3], "123")
            self.assertEqual(argv[-1], "status,conclusion,jobs")
            return subprocess.CompletedProcess(argv, 0, json.dumps(self.view), "")
        if argv[:3] == ["gh", "run", "download"]:
            self.assertEqual(argv[3], "123")
            self.assertEqual(argv[argv.index("--name") + 1], f"cito-candidate-{self.sha}")
            if self.download_available:
                (Path(argv[argv.index("--dir") + 1]) / "candidate.zip").write_bytes(b"exact commit candidate")
            return subprocess.CompletedProcess(argv, int(not self.download_available), "", "")
        self.fail(f"Unexpected external command: {argv}")

    def lookup(self):
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            release.candidate(self.sha, self.destination)
        return stdout.getvalue()

    def test_candidate_requires_exact_main_push_commit(self):
        self.pages = [{"workflow_runs": [
            self.workflow(head_sha="b" * 40), self.workflow(head_branch="feature"), self.workflow(event="pull_request"),
        ]}]
        self.assertEqual(self.lookup(), "reused=false\n")
        self.assertEqual(len(self.commands), 1)

    def test_prior_failed_run_blocks_reuse_or_rebuild(self):
        self.pages.append({"workflow_runs": [self.workflow(id=122, conclusion="failure")]})
        for status, conclusion in (("completed", "success"), ("completed", "cancelled"), ("in_progress", None)):
            with self.subTest(status=status, conclusion=conclusion):
                self.commands.clear()
                self.pages[0] = {"workflow_runs": [self.workflow(status=status, conclusion=conclusion)]}
                with self.assertRaisesRegex(release.ReleaseError, "selected source commit did not pass"):
                    self.lookup()
                self.assertEqual(len(self.commands), 1)

    def test_matching_job_must_have_passed_and_lookalike_names_do_not_count(self):
        lookalike = {**self.job, "name": self.job["name"] + " (optional)"}
        for jobs in ([lookalike], [lookalike, {**self.job, "conclusion": "skipped"}],
                     [{**self.job, "status": "in_progress"}]):
            with self.subTest(jobs=jobs):
                self.commands.clear()
                self.view["jobs"] = jobs
                with self.assertRaisesRegex(release.ReleaseError, "required job"):
                    self.lookup()
                self.assertEqual([argv[2] for argv in self.commands], ["--paginate", "view"])
        self.view["jobs"] = [lookalike, self.job]
        self.assertEqual(self.lookup(), "reused=true\n")
        self.assertEqual(self.destination.read_bytes(), b"exact commit candidate")

    def test_expired_artifact_and_cancelled_ci_can_rebuild(self):
        self.download_available = False
        self.assertEqual(self.lookup(), "reused=false\n")
        self.view["conclusion"] = "cancelled"
        self.commands.clear()
        self.assertEqual(self.lookup(), "reused=false\n")
        self.assertEqual([argv[2] for argv in self.commands], ["--paginate", "view"])

    def test_watch_failure_defers_to_authoritative_run_status(self):
        self.pages[0] = {"workflow_runs": [self.workflow(status="in_progress", conclusion=None)]}
        self.view.update(status="in_progress", conclusion=None)
        with self.assertRaisesRegex(release.ReleaseError, "has not completed"):
            self.lookup()
        self.view.update(status="completed", conclusion="failure")
        with self.assertRaisesRegex(release.ReleaseError, "selected source commit did not pass"):
            self.lookup()
        self.view.update(status="completed", conclusion="success")
        self.assertEqual(self.lookup(), "reused=true\n")

    def test_watch_timeout_reports_ci_wait_without_allowing_a_rebuild(self):
        self.pages[0] = {"workflow_runs": [self.workflow(status="queued", conclusion=None)]}
        self.watch_timeout = True
        argv = ["release.py", "candidate", self.sha, str(self.destination)]
        with patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as stderr, contextlib.redirect_stdout(io.StringIO()) as stdout:
            self.assertEqual(release.main(), 1)
        self.assertIn("Timed out after 20 minutes waiting for CI", stderr.getvalue())
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual([command[2] for command in self.commands], ["--paginate", "watch"])


if __name__ == "__main__":
    unittest.main()
