use super::support::*;
use serde_json::{json, Value};
use std::fs;
#[test]
fn website_clipboard_and_workflow_previews_preserve_dom_interactions() {
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
fn distribution_versions_native_manifests_cask_and_signed_ui() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let r = root();
    let producer = r.join("packaging/release.py");
    let producer = producer.to_str().unwrap();
    let (ok, out, err) = f.command("python3", &[producer, "check"], "");
    assert!(ok, "{out} {err}");
    assert_eq!(out.trim(), env!("CARGO_PKG_VERSION"));
    let cask = f.dir.join("cito.rb");
    let checksum = "a".repeat(64);
    let (ok, out, err) = f.command(
        "python3",
        &[
            producer,
            "render-cask",
            "--sha256",
            &checksum,
            "--output",
            cask.to_str().unwrap(),
        ],
        "",
    );
    assert!(ok, "{out} {err}");
    assert!(
        f.command("/usr/bin/ruby", &["-c", cask.to_str().unwrap()], "")
            .0
    );
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
    for id in ["codex", "pi", "claude", "telegram"] {
        let source = fs::read(root().join("plugins").join(id).join("icon.png")).unwrap();
        assert_eq!(
            fs::read(
                app.join("Contents/Resources/plugins")
                    .join(id)
                    .join("icon.png")
            )
            .unwrap(),
            source
        );
        assert_eq!(
            fs::read(f.state.join("runtime/plugins").join(id).join("icon.png")).unwrap(),
            source
        );
    }
    // Cargo tests use debug executables. Release archive size is checked after release staging.
    assert!(v["bytes"].as_u64().unwrap() > 100_000);
    let cask_bin = f.dir.join("cask-bin");
    fs::create_dir_all(&cask_bin).unwrap();
    for name in ["cito", "cito-mcp"] {
        std::os::unix::fs::symlink(app.join("Contents/MacOS/Cito"), cask_bin.join(name)).unwrap();
    }
    for path in [
        app.join("Contents/MacOS/Cito"),
        f.state.join("bin/cito"),
        cask_bin.join("cito"),
        cask_bin.join("cito-mcp"),
    ] {
        let (ok, out, err) = f.command(path.to_str().unwrap(), &["--version"], "");
        assert!(ok, "{err}");
        assert_eq!(out, format!("cito v{}\n", env!("CARGO_PKG_VERSION")));
    }
    // A relative hop must retain the original command name for MCP dispatch.
    let relative = cask_bin.join("relative-target");
    std::os::unix::fs::symlink(app.join("Contents/MacOS/Cito"), &relative).unwrap();
    let initialize = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"cask-symlink\",\"version\":\"1.0\"}}}\n";
    let (ok, out, err) = f.command(cask_bin.join("cito-mcp").to_str().unwrap(), &[], initialize);
    assert!(ok, "{out} {err}");
    let response: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    fs::remove_file(cask_bin.join("cito-mcp")).unwrap();
    std::os::unix::fs::symlink("relative-target", cask_bin.join("cito-mcp")).unwrap();
    let (ok, out, err) = f.command(cask_bin.join("cito-mcp").to_str().unwrap(), &[], initialize);
    assert!(ok, "{out} {err}");
    let response: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
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
    let settings: Value =
        serde_json::from_slice(&fs::read(f.home.join(".claude/settings.json")).unwrap()).unwrap();
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        std::path::Path::new(command),
        f.state.join("runtime/plugins/claude/bin/cito-claude-hook")
    );
    let (ok, out, err) = f.command(command, &[], "{\"hook_event_name\":\"Stop\"}\n");
    assert!(ok, "{err}");
    assert_eq!(out, "{}\n");
}
#[test]
fn website_links_resolve_under_pages_subpath_and_png_assets_decode() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
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
            html.contains(&format!(
                "name=\"application-version\" content=\"{}\"",
                env!("CARGO_PKG_VERSION")
            )),
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
    let architecture = fs::read_to_string(site.join("docs/architecture.html")).unwrap();
    assert!(architecture.contains("src=\"../assets/art/architecture-topology.svg\""));
    let diagram = fs::read_to_string(site.join("assets/art/architecture-topology.svg")).unwrap();
    for attribute in [
        "viewBox=\"0 0 960 420\"",
        "width=\"960\"",
        "height=\"420\"",
        "aria-labelledby=\"title desc\"",
    ] {
        assert!(
            diagram.contains(attribute),
            "Missing SVG image contract: {attribute}"
        );
    }
    for (path, width, height) in [
        ("apple-touch-icon.png", 180, 180),
        ("assets/og-card.png", 1200, 630),
        ("assets/screenshots/menu-dark.png", 760, 684),
        ("assets/screenshots/directory-dark.png", 1120, 880),
        ("assets/screenshots/activity-dark.png", 1120, 880),
        ("assets/screenshots/settings-apps-dark.png", 1040, 920),
        ("assets/screenshots/settings-channels-dark.png", 1040, 920),
        ("assets/screenshots/settings-hubs-dark.png", 1040, 920),
        ("assets/screenshots/banner-dark.png", 776, 272),
        ("assets/screenshots/wake-card-dark.png", 776, 272),
    ] {
        let bytes = fs::read(site.join(path)).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), width);
        assert_eq!(
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            height
        );
        // Decode the pixel payload, not just the PNG metadata, through the native codec.
        let decoded = f.dir.join("decoded.bmp");
        let (ok, out, err) = f.command(
            "/usr/bin/sips",
            &[
                "-s",
                "format",
                "bmp",
                site.join(path).to_str().unwrap(),
                "--out",
                decoded.to_str().unwrap(),
            ],
            "",
        );
        assert!(ok, "{path}: {out}\n{err}");
        let bitmap = fs::read(&decoded).unwrap();
        assert_eq!(&bitmap[..2], b"BM", "{path}");
        assert_eq!(
            u32::from_le_bytes(bitmap[18..22].try_into().unwrap()),
            width
        );
        assert_eq!(
            i32::from_le_bytes(bitmap[22..26].try_into().unwrap()).unsigned_abs(),
            height
        );
        assert!(bitmap.len() >= (width * height * 3) as usize, "{path}");
        fs::remove_file(decoded).unwrap();
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
