use super::*;
use std::fs;

#[test]
fn packaged_channel_plugin_roots_resolve_and_preserve_override_order() {
    let dir = std::env::temp_dir().join(format!(
        "cito-plugin-roots-{}",
        crate::plugins::random_uuid_v4()
    ));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let app = dir.join("Cito.app/Contents");
    std::fs::create_dir_all(app.join("MacOS")).unwrap();
    for (root, id, display) in [
        (
            app.join("Resources/plugins"),
            "bundle-fixture",
            "App bundle",
        ),
        (dir.join("builtin"), "brew-fixture", "Package builtin"),
        (dir.join("extra"), "extra-fixture", "Extra"),
        (dir.join("data/plugins"), "data-fixture", "Data"),
        (
            app.join("Resources/plugins"),
            "override-fixture",
            "App override",
        ),
        (dir.join("builtin"), "override-fixture", "Builtin override"),
        (
            dir.join("data/runtime/plugins"),
            "override-fixture",
            "Runtime override",
        ),
        (dir.join("first"), "override-fixture", "First override"),
        (dir.join("second"), "override-fixture", "Second override"),
        (dir.join("extra"), "override-fixture", "Extra override"),
        (
            dir.join("data/plugins"),
            "override-fixture",
            "Data override",
        ),
    ] {
        let plugin = root.join(id);
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"id":id,"kind":"human-channel","displayName":display,
                "tier":"experimental","command":["./bin/cito-plugin-fixture"]})
            .to_string(),
        )
        .unwrap();
    }
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core::hub_plugins::tests::packaged_roots_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("CITO_PLUGIN_ROOT_TEST", &dir)
        .env("CITO_BUILTIN_PLUGINS_DIR", dir.join("builtin"))
        .env("CITO_PLUGIN_DIR", dir.join("extra"))
        .env(
            "CITO_PLUGIN_PATH",
            format!(
                "{}:{}",
                dir.join("first").display(),
                dir.join("second").display()
            ),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[ignore = "isolated environment worker invoked by the packaged roots test"]
fn packaged_roots_worker() {
    let dir = PathBuf::from(std::env::var("CITO_PLUGIN_ROOT_TEST").unwrap());
    let data = dir.join("data");
    let assert_plugin = |plugins: &[ChannelPlugin], id: &str, expected: PathBuf| {
        let plugin = plugins.iter().find(|p| p.id == id).unwrap();
        assert_eq!(
            plugin.dir.canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
    };
    let plugins = discover(&default_roots(&data)).unwrap();
    assert_plugin(&plugins, "brew-fixture", dir.join("builtin/brew-fixture"));
    assert_plugin(&plugins, "extra-fixture", dir.join("extra/extra-fixture"));
    assert_plugin(&plugins, "data-fixture", data.join("plugins/data-fixture"));

    // The app layout is data only: no copied executable or launched application.
    let exe = dir.join("Cito.app/Contents/MacOS/cito");
    let roots = roots_for_executable(&data, Some(&exe));
    let plugins = discover(&roots).unwrap();
    assert_plugin(
        &plugins,
        "bundle-fixture",
        dir.join("Cito.app/Contents/Resources/plugins/bundle-fixture"),
    );
    for relative in [
        "first",
        "second",
        "extra",
        "data/plugins",
        "data/runtime/plugins",
        "builtin",
        "Cito.app/Contents/Resources/plugins",
    ] {
        let expected = dir.join(relative).join("override-fixture");
        let plugins = discover(&roots).unwrap();
        assert_plugin(&plugins, "override-fixture", expected.clone());
        assert_eq!(
            plugins
                .iter()
                .filter(|p| p.id == "override-fixture")
                .count(),
            1
        );
        fs::remove_dir_all(expected).unwrap();
    }
}
