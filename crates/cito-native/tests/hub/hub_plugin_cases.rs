use super::*;
use std::fs;
use std::process::Command;
struct Descendant(i32);
impl Drop for Descendant {
    fn drop(&mut self) {
        if self.0 > 0 {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn child_exit_is_not_masked_by_a_descendants_inherited_stdout() {
    let root = crate::core::hub::TestDirectory(std::env::temp_dir().join(format!(
        "cito-child-exit-{}",
        crate::plugins::random_uuid_v4()
    )));
    let dir = root.join("sample");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("driver.py");
    fs::write(&script,"import subprocess,sys,pathlib\np=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])\npathlib.Path('descendant').write_text(str(p.pid))\n").unwrap();
    let plugin = ChannelPlugin {
        is_builtin: false,
        id: "sample".into(),
        dir: dir.clone(),
        manifest: json!({}),
        command: vec!["python3".into(), script.to_string_lossy().into_owned()],
    };
    let (tx, rx) = mpsc::channel();
    let process = ChannelProcess::spawn(
        &plugin,
        Arc::new(move |frame| {
            tx.send(frame).unwrap();
            None
        }),
    )
    .unwrap();
    let exit = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(exit["op"], "process_exit");
    assert_eq!(exit["error"], "Plugin subprocess exited");
    let pid = fs::read_to_string(dir.join("descendant"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let mut cleanup = Descendant(pid);
    let exited = crate::support::child_exit_fd(pid as u32).unwrap();
    assert!(
        !process.alive(),
        "A descendant holding stdout must not keep the OS child alive"
    );
    process.shutdown();
    use std::os::fd::AsRawFd;
    assert!(crate::support::poll_until(
        &mut [libc::pollfd {
            fd: exited.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0
        }],
        Some(Instant::now() + Duration::from_secs(1))
    )
    .unwrap());
    cleanup.0 = 0;
    drop(cleanup);
}

#[test]
fn shutdown_after_reap_leaves_descendants_unsignaled_and_interrupts_idle_reader() {
    let root = crate::core::hub::TestDirectory(
        std::env::temp_dir().join(format!("cito-reaped-{}", crate::plugins::random_uuid_v4())),
    );
    let dir = root.join("sample");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("driver.py");
    fs::write(&script,"import subprocess,sys,pathlib\np=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])\npathlib.Path('descendant').write_text(str(p.pid))\n").unwrap();
    let plugin = ChannelPlugin {
        is_builtin: false,
        id: "sample".into(),
        dir: dir.clone(),
        manifest: json!({}),
        command: vec!["python3".into(), script.to_string_lossy().into_owned()],
    };
    let process = ChannelProcess::spawn(&plugin, Arc::new(|_| None)).unwrap();
    assert!(crate::support::wait_child_exit(
        &process.child.lock().unwrap(),
        Duration::from_secs(2)
    )
    .unwrap());
    let pid = fs::read_to_string(dir.join("descendant"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let cleanup = Descendant(pid);
    assert!(process.child.lock().unwrap().try_wait().unwrap().is_some());
    let started = Instant::now();
    process.shutdown();
    assert!(started.elapsed() < Duration::from_millis(200));
    // A post-reap group SIGKILL would kill this still-live descendant.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    drop(cleanup);
}
#[test]
fn native_and_hub_manifests_share_validation_and_script_entrypoint_rules() {
    let root = std::env::temp_dir().join(format!(
        "cito-manifest-parity-{}",
        crate::plugins::random_uuid_v4()
    ));
    let roots = vec![(root.clone(), false)];
    let dir = root.join("sample");
    fs::create_dir_all(&dir).unwrap();
    let valid = json!({"id":"sample","kind":"human","displayName":"Sample","command":["./driver"]});
    for (field, invalid) in [
        ("id", json!("different")),
        ("displayName", json!("x".repeat(65))),
        ("displayName", json!("Bad\n")),
        ("displayName", json!("Bad\u{0085}name")),
    ] {
        let mut manifest = valid.clone();
        manifest[field] = invalid;
        fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
        assert!(super::super::plugins::parse_manifest_dir(&dir, false).is_err());
        assert!(discover(&roots, Some(&root)).unwrap().is_empty());
    }
    let mut script = valid.clone();
    script.as_object_mut().unwrap().remove("command");
    fs::write(dir.join("index.js"), "process.exit(0)").unwrap();
    fs::write(dir.join("plugin.json"), script.to_string()).unwrap();
    assert!(super::super::plugins::parse_manifest_dir(&dir, true).is_err());
    let native = super::super::plugins::parse_manifest_dir(&dir, false).unwrap();
    let hub = discover(&roots, Some(&root)).unwrap();
    assert!(discover(&[(root.clone(), true)], Some(&root))
        .unwrap()
        .is_empty());
    assert_eq!(hub[0].command, native.command);
    script["scope"] = json!("user");
    fs::write(dir.join("plugin.json"), script.to_string()).unwrap();
    assert!(discover(&roots, Some(&root)).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_root_aliases_retire_staged_copies_without_retiring_builtin_roots() {
    let dir = std::env::temp_dir().join(format!(
        "cito-hub-root-alias-{}",
        crate::plugins::random_uuid_v4()
    ));
    let user = dir.join("state/plugins");
    let alias = dir.join("alias");
    fs::create_dir_all(&user).unwrap();
    std::os::unix::fs::symlink(&user, &alias).unwrap();
    let staged = user.join("telegram");
    let mut manifest: Value =
        serde_json::from_str(include_str!("../../../../plugins/telegram/plugin.json")).unwrap();
    manifest["version"] = json!("0.0.0");
    let write_staged = |value: &Value| {
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("plugin.json"), value.to_string()).unwrap();
    };
    for root in [alias.clone(), user.join("../plugins")] {
        write_staged(&manifest);
        assert!(discover(&[(root, false)], Some(&user)).unwrap().is_empty());
        assert!(!staged.exists());
    }
    write_staged(&manifest);
    let builtin = discover(&[(alias.clone(), true)], Some(&user)).unwrap();
    assert_eq!(builtin.len(), 1);
    assert_eq!(builtin[0].id, "telegram");
    assert!(
        staged.exists(),
        "A builtin fallback is never a user-staged copy"
    );
    for (field, value) in [
        ("command", json!(["./bin/cito-custom"])),
        ("displayName", json!("My channel")),
    ] {
        let mut custom = manifest.clone();
        custom[field] = value;
        write_staged(&custom);
        let found = discover(&[(alias.clone(), false)], Some(&user)).unwrap();
        assert_eq!(found.len(), 1);
        assert!(!found[0].is_builtin);
        assert_eq!(found[0].manifest, custom);
        assert!(staged.join("plugin.json").is_file());
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hub_root_subdirectories_have_deterministic_validation_order() {
    let dir = std::env::temp_dir().join(format!(
        "cito-hub-discovery-order-{}",
        crate::plugins::random_uuid_v4()
    ));
    for name in ["z-invalid", "a-invalid"] {
        fs::create_dir_all(dir.join(name)).unwrap();
        fs::write(dir.join(name).join("plugin.json"), "{malformed").unwrap();
    }
    let valid = dir.join("sample");
    fs::create_dir_all(&valid).unwrap();
    fs::write(
        valid.join("plugin.json"),
        json!({"id":"sample","displayName":"Sample","kind":"human-channel","command":["./driver"]})
            .to_string(),
    )
    .unwrap();
    let found = discover(&[(dir.clone(), false)], None).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<Vec<_>>(),
        ["sample"]
    );
    fs::remove_dir_all(dir).unwrap();
}

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
    let runtime = dir.join("data/runtime/plugins/telegram");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(runtime.join("plugin.json"), "{malformed").unwrap();
    let user = dir.join("data/plugins/telegram");
    fs::create_dir_all(&user).unwrap();
    fs::write(
        user.join("plugin.json"),
        json!({"id":"telegram","kind":"human","displayName":"User channel","command":["./driver"]})
            .to_string(),
    )
    .unwrap();
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
    let plugins = discover(
        &super::super::plugins::plugin_roots_for_executable(
            &data,
            std::env::current_exe().ok().as_deref(),
        ),
        Some(&data.join("plugins")),
    )
    .unwrap();
    assert_plugin(&plugins, "brew-fixture", dir.join("builtin/brew-fixture"));
    assert_plugin(&plugins, "extra-fixture", dir.join("extra/extra-fixture"));
    assert_plugin(&plugins, "data-fixture", data.join("plugins/data-fixture"));

    // The app layout is data only: no copied executable or launched application.
    let exe = dir.join("Cito.app/Contents/MacOS/cito");
    let roots = super::super::plugins::plugin_roots_for_executable(&data, Some(&exe));
    let plugins = discover(&roots, Some(&data.join("plugins"))).unwrap();
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
        "builtin",
        "data/runtime/plugins",
        "Cito.app/Contents/Resources/plugins",
    ] {
        let expected = dir.join(relative).join("override-fixture");
        let plugins = discover(&roots, Some(&data.join("plugins"))).unwrap();
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
    let user = data.join("plugins/telegram");
    assert_plugin(
        &discover(&roots, Some(&data.join("plugins"))).unwrap(),
        "telegram",
        user.clone(),
    );
    fs::write(user.join("plugin.json"), json!({"id":"telegram","kind":"agent","displayName":"Agent override","command":["./driver"]}).to_string()).unwrap();
    assert!(!discover(&roots, Some(&data.join("plugins")))
        .unwrap()
        .iter()
        .any(|p| p.id == "telegram"));
    fs::remove_dir_all(user).unwrap();
    assert!(
        discover(&roots, Some(&data.join("plugins"))).is_ok(),
        "An unshadowed malformed manifest must not abort other channel discovery"
    );
}
