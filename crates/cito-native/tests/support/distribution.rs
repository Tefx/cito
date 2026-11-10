use super::support::*;
use serde_json::{json, Value};
use std::fs;
#[test]
fn website_clipboard_promises_failure_retry_and_duplicate_suppression() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let file = root().join("crates/cito-native/tests/support/site-clipboard.mjs");
    let (ok, out, err) = f.command("node", &[file.to_str().unwrap()], "");
    assert!(ok, "{out}\n{err}");
}
#[test]
fn release_linker_stages_production_binaries_but_never_libtest_harnesses() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let package = f.dir.join("package");
    let bin = package.join("bin");
    let release = package.join("target/release");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(release.join("deps")).unwrap();
    let linker = bin.join("cito-linker");
    fs::copy(root().join("bin/cito-linker"), &linker).unwrap();
    let source = package.join("main.c");
    fs::write(
        &source,
        "#include <stdio.h>\nint main(void) { puts(\"production\"); return 0; }\n",
    )
    .unwrap();
    let object = package.join("main.o");
    let archive = package.join("libtest-fixture.rlib");
    for (program, args) in [
        (
            "/usr/bin/cc",
            vec![
                "-c",
                source.to_str().unwrap(),
                "-o",
                object.to_str().unwrap(),
            ],
        ),
        (
            "/usr/bin/ar",
            vec!["-crs", archive.to_str().unwrap(), object.to_str().unwrap()],
        ),
    ] {
        let (ok, out, err) = f.command(program, &args, "");
        assert!(ok, "{out}\n{err}");
    }
    let linked = release.join("deps/cito_native-fixture");
    let staged = bin.join("cito-native-darwin-arm64");
    let (ok, out, err) = f.command(
        linker.to_str().unwrap(),
        &[
            source.to_str().unwrap(),
            archive.to_str().unwrap(),
            "-o",
            linked.to_str().unwrap(),
        ],
        "",
    );
    assert!(ok, "{out}\n{err}");
    assert!(linked.is_file());
    assert!(
        !staged.exists(),
        "libtest invocation staged a production executable"
    );

    // An older canonical release must never replace the newly linked executable.
    let canonical = release.join("cito-native");
    fs::write(&canonical, "stale canonical release").unwrap();
    let (ok, out, err) = f.command(
        linker.to_str().unwrap(),
        &[source.to_str().unwrap(), "-o", linked.to_str().unwrap()],
        "",
    );
    assert!(ok, "{out}\n{err}");
    assert!(err.is_empty(), "re-signing emitted linker stderr: {err}");
    let (ok, out, err) = f.command(
        "/usr/bin/codesign",
        &["--display", "--verbose=4", linked.to_str().unwrap()],
        "",
    );
    assert!(ok, "{out}\n{err}");
    assert!(err.contains("Signature=adhoc"), "{err}");
    assert_eq!(
        f.command(staged.to_str().unwrap(), &[], ""),
        (true, "production\n".into(), String::new())
    );
    assert!(
        f.command(
            "/usr/bin/codesign",
            &["--verify", staged.to_str().unwrap()],
            ""
        )
        .0
    );
    fs::copy(&linked, &canonical).unwrap();
    let (ok, out, err) = f.command(
        linker.to_str().unwrap(),
        &[source.to_str().unwrap(), "-o", linked.to_str().unwrap()],
        "",
    );
    assert!(ok, "{out}\n{err}");
    assert!(err.is_empty(), "re-signing emitted linker stderr: {err}");
    assert_eq!(
        f.command(staged.to_str().unwrap(), &[], "").1,
        "production\n"
    );
    let production = fs::read(&staged).unwrap();
    fs::write(&source, "int main(void) { return 7; }\n").unwrap();
    let (ok, out, err) = f.command(
        linker.to_str().unwrap(),
        &[
            source.to_str().unwrap(),
            archive.to_str().unwrap(),
            "-o",
            linked.to_str().unwrap(),
        ],
        "",
    );
    assert!(ok, "{out}\n{err}");
    assert_eq!(
        fs::read(&staged).unwrap(),
        production,
        "libtest invocation overwrote staged production binary"
    );

    // Fat LTO absorbs libtest into an object, so its archive is absent from linker arguments.
    let rustc = std::path::Path::new(env!("CARGO")).with_file_name("rustc");
    let rust_source = package.join("harness.rs");
    fs::write(
        &rust_source,
        "#[test] fn passes() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    let (ok, out, err) = f.command(
        rustc.to_str().unwrap(),
        &[
            "--test",
            "-C",
            "lto=true",
            "-C",
            "opt-level=z",
            "-C",
            "strip=symbols",
            "-C",
            &format!("linker={}", linker.display()),
            rust_source.to_str().unwrap(),
            "-o",
            linked.to_str().unwrap(),
        ],
        "",
    );
    assert!(ok, "{out}\n{err}");
    let (ok, out, err) = f.command(linked.to_str().unwrap(), &["--list"], "");
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("passes: test"));
    assert_eq!(
        fs::read(&staged).unwrap(),
        production,
        "LTO test harness overwrote staged production binary"
    );

    // Contaminate only the staged copy so the real signer rejects it after a successful link.
    let tools = f.dir.join("tools");
    script(
        &tools.join("cp"),
        "#!/bin/sh\nset -eu\n/bin/cp \"$@\"\n/usr/bin/xattr -w com.apple.ResourceFork fixture \"$2\"\n",
    );
    f.env.insert(
        "PATH".into(),
        format!("{}:{}", tools.display(), f.env["PATH"]),
    );
    let (ok, out, err) = f.command(
        linker.to_str().unwrap(),
        &[source.to_str().unwrap(), "-o", linked.to_str().unwrap()],
        "",
    );
    assert!(!ok, "signing failure was swallowed: {out}\n{err}");
    assert!(
        err.contains("resource fork, Finder information, or similar detritus not allowed"),
        "signing diagnostic was swallowed: {err}"
    );
    assert_eq!(
        fs::read(&staged).unwrap(),
        production,
        "failed signing replaced the staged production binary"
    );
    assert!(fs::read_dir(&bin).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".tmp.")));
}
#[test]
fn distribution_versions_native_manifests_formula_and_signed_ui() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let r = root();
    let formula = r.join("packaging/homebrew/cito.rb");
    assert!(
        f.command("/usr/bin/ruby", &["-c", formula.to_str().unwrap()], "")
            .0
    );
    let text = fs::read_to_string(formula).unwrap();
    assert!(text.contains("version \"0.20.2\""));
    assert!(text.contains("v0.20.2"));
    assert!(!text.contains("depends_on \"node\""));
    assert!(
        text.contains("(prefix/\"Cito.app\").install \"Contents\""),
        "Formula must handle Homebrew's single-top-level-directory stripping when unpacking Cito.app"
    );
    for path in [
        ".agents/plugins/marketplace.json",
        "plugins/codex/.codex-plugin/plugin.json",
        "plugins/claude/.claude-plugin/plugin.json",
        "plugins/pi/package.json",
        "plugins/pi/plugin.json",
        "plugins/codex/plugin.json",
        "plugins/claude/plugin.json",
        "plugins/human/plugin.json",
        "plugins/telegram/plugin.json",
    ] {
        let v: Value = serde_json::from_str(&fs::read_to_string(r.join(path)).unwrap()).unwrap();
        assert_eq!(v["version"], "0.20.2", "{path}");
    }
    let v: Value =
        serde_json::from_str(&fs::read_to_string(r.join("plugins/pi/package.json")).unwrap())
            .unwrap();
    assert_eq!(v["private"], true);
    assert_eq!(v["type"], "module");
    assert_eq!(v["pi"]["extensions"], json!(["./cito-bridge.ts"]));
    assert!(v.get("dependencies").is_none());
    assert!(v.get("devDependencies").is_none());
    let ui = r.join("bin/cito-ui-darwin-arm64");
    let bytes = fs::read(&ui).unwrap();
    assert_eq!(&bytes[..4], &[0xcf, 0xfa, 0xed, 0xfe]);
    assert_eq!(&bytes[4..8], &[12, 0, 0, 1]);
    assert!(
        f.command("/usr/bin/codesign", &["--verify", ui.to_str().unwrap()], "")
            .0
    );
    assert_eq!(
        bytes,
        fs::read(r.join("plugins/human/bin/cito-ui")).unwrap()
    );
}
#[test]
fn relocatable_signed_app_and_installed_shims_work_without_node() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    f.env
        .insert("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let (ok, out, err) = f.command(
        NATIVE,
        &["setup", "--enable", "claude", "--no-login-item", "--json"],
        "",
    );
    assert!(ok, "{out} {err}");
    let (ok, out, err) = f.command(
        NATIVE,
        &[
            "setup",
            "--build-app",
            f.dir.join("Apps").to_str().unwrap(),
            "--json",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let app = std::path::Path::new(v["appPath"].as_str().unwrap());
    // Cargo tests use debug executables. Release archive size is checked after release staging.
    assert!(v["bytes"].as_u64().unwrap() > 100_000);
    for path in [app.join("Contents/MacOS/Cito"), f.state.join("bin/cito")] {
        let (ok, out, err) = f.command(path.to_str().unwrap(), &["--version"], "");
        assert!(ok, "{err}");
        assert_eq!(out, "cito v0.20.2\n");
    }
    assert!(
        f.command(
            "/usr/bin/codesign",
            &["--verify", "--deep", app.to_str().unwrap()],
            ""
        )
        .0
    );
    assert_eq!(
        fs::read(app.join("Contents/Resources/AppIcon.icns")).unwrap(),
        fs::read(root().join("bin/AppIcon.icns")).unwrap()
    );
    let (ok, out, err) = f.command(
        f.state
            .join("plugins/claude/bin/cito-claude-hook")
            .to_str()
            .unwrap(),
        &[],
        "{\"hook_event_name\":\"Stop\"}\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "{}\n");
}
#[test]
fn website_links_resolve_under_pages_subpath_and_png_assets_decode() {
    let _lock = serial();
    let _f = Fixture::new(json!({}));
    let site = root().join("site");
    for page in [
        "index.html",
        "docs/guide.html",
        "docs/plugins.html",
        "docs/architecture.html",
    ] {
        let file = site.join(page);
        let html = fs::read_to_string(&file).unwrap();
        assert!(
            html.contains("name=\"application-version\" content=\"0.20.2\""),
            "{page}"
        );
        for attr in ["href=\"", "src=\""] {
            for tail in html.split(attr).skip(1) {
                let reference = tail.split('"').next().unwrap();
                if reference.starts_with("http")
                    || reference.starts_with("mailto:")
                    || reference.starts_with('#')
                    || reference.starts_with("data:")
                {
                    continue;
                }
                assert!(!reference.starts_with('/'), "{page}: {reference}");
                let local = reference.split(['?', '#']).next().unwrap();
                let path = file.parent().unwrap().join(local);
                assert!(path.exists(), "Broken link {page}: {reference}");
                assert!(path
                    .canonicalize()
                    .unwrap()
                    .starts_with(site.canonicalize().unwrap()));
            }
        }
        assert!(html.contains("https://tefx.github.io/cito/assets/og-card.png"));
    }
    for (path, width, height) in [
        ("apple-touch-icon.png", 180, 180),
        ("assets/og-card.png", 1200, 630),
    ] {
        let bytes = fs::read(site.join(path)).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), width);
        assert_eq!(
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            height
        );
    }
    let css = fs::read_to_string(site.join("styles/main.css")).unwrap();
    for rule in [
        "@media (max-width: 640px)",
        ".nav-inner",
        ".nav-links",
        "minmax(min(100%, 19rem), 1fr)",
    ] {
        assert!(css.contains(rule), "Missing mobile layout rule: {rule}");
    }
    assert!(site.join(".nojekyll").exists());
    assert!(!site.join("CNAME").exists());
}
fn luminance(hex: &str) -> f64 {
    let channels: Vec<_> = (0..6)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.)
        .map(|v| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]
}
#[test]
fn brand_svg_self_backing_and_foreground_contrast_on_both_readme_themes() {
    let _lock = serial();
    let _f = Fixture::new(json!({}));
    let r = root();
    for path in [
        "site/assets/brand/cito-mark.svg",
        "site/assets/brand/cito-logo-dark.svg",
        "site/assets/brand/cito-logo-light.svg",
        "site/favicon.svg",
    ] {
        let svg = fs::read_to_string(r.join(path)).unwrap();
        let backing = if path.ends_with("favicon.svg") {
            "0C0E12"
        } else {
            "14171F"
        };
        assert!(svg.contains(&format!("fill=\"#{backing}\"")), "{path}");
        for fg in ["F3F4F6", "F59E0B"] {
            assert!(svg.contains(fg));
            assert!((luminance(fg) + 0.05) / (luminance(backing) + 0.05) > 4.5);
        }
        assert!(svg.contains("cx=\"8\" cy=\"9\" r=\"1.65\""));
        assert!(svg.contains("M 12.09 5.32 A 5.50 5.50 0 1 0 12.09 12.68"));
    }
    assert!(fs::read_to_string(r.join("README.md"))
        .unwrap()
        .contains("src=\"site/assets/brand/cito-logo-dark.svg\""));
}
