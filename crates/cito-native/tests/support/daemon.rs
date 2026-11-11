use super::support::*;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::fs;

#[test]
fn u1_discovery_hides_unconfigured_apps_and_refreshes_setup_lifecycle() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let script = f.dir.join("endpoint.cjs");
    fs::write(&script, include_str!("endpoint.cjs")).unwrap();
    let endpoint = f.spawn(
        "node",
        &[script.to_str().unwrap()],
        &[("FIXTURE_HARNESS", "pi")],
    );
    wait(|| f.frames(endpoint).iter().any(|v| v["op"] == "ready"));
    assert_eq!(
        f.plugin(PI, json!({"op":"discover_sessions"}))["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.daemon(&[]);
    let available = || f.action(json!({"op":"get_snapshot"})).1["available"].clone();
    assert_eq!(available(), json!([]));
    assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
    wait(|| {
        available()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "pi")
    });
    assert_eq!(
        f.action(json!({"op":"setup_remove_app","harness":"pi"})).0,
        200
    );
    assert_eq!(available(), json!([]));
    assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
    assert_eq!(f.action(json!({"op":"setup_disconnect_all"})).0, 200);
    assert_eq!(available(), json!([]));
}

#[test]
fn u1_synthetic_sessions_follow_explicit_configuration_without_global_hardcoding() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered");
    f.daemon(&[]);
    let snapshot = || f.action(json!({"op":"get_snapshot"})).1;
    wait(|| {
        snapshot()["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["harness"] == "fixture")
    });
    let options = f.state.join("plugins/fixture/options.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&options).unwrap()).unwrap();
    config["configured"] = json!(false);
    fs::write(&options, config.to_string()).unwrap();
    assert_eq!(snapshot()["available"], json!([]));
    config["configured"] = json!(true);
    fs::write(&options, config.to_string()).unwrap();
    assert!(snapshot()["available"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["harness"] == "fixture"));
}

#[test]
fn u8_token_auth_generates_and_persists_token_while_sharing_is_off() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[("CITO_TAILSCALE_HOST", "mac-studio.tailnet.ts.net.")]);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}))
            .0,
        200
    );
    let host = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert_eq!(host["enabled"], false);
    assert_eq!(host["running"], false);
    assert_eq!(host["token"].as_str().unwrap().len(), 64);
    assert_eq!(
        host["endpoint_url"],
        "http://mac-studio.tailnet.ts.net:4291"
    );
    assert!(host["invite_command"]
        .as_str()
        .unwrap()
        .contains("--url http://mac-studio.tailnet.ts.net:4291"));
    let saved: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(saved["host_hub"]["token"], host["token"]);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false,"auth_mode":"token"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"]["token"],
        host["token"]
    );
}

#[test]
fn lifecycle_channel_test_failures_emit_results_before_configured_refresh() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let ui = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    assert_eq!(f.action(json!({"op":"channel_save","channel_id":"channel","enabled":true,"values":{"credential":"saved","address":"room"}})).0, 200);
    let frames = || -> Vec<Value> {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    for op in ["channel_test", "test_human_channel"] {
        for (id, expected_error, expected_status) in [
            ("channel", "Invalid token", 200),
            (
                "missing",
                "INVALID_INPUT: Unknown human channel plugin: missing",
                400,
            ),
        ] {
            let before = frames().len();
            let action = json!({"op":op,"channel_id":id,"values":if id == "channel" {
                json!({"credential":"invalid"})
            } else {
                json!({"botToken":"x"})
            }});
            let (status, result) = f.action(action);
            assert_eq!(status, expected_status, "{result}");
            assert_eq!(result["ok"], false);
            assert_eq!(result["error"], expected_error);
            wait(|| {
                let frames = frames();
                let Some(index) = frames.iter().enumerate().skip(before).find_map(|(i, v)| {
                    (v["op"] == "channel_test_result" && v["channel_id"] == id).then_some(i)
                }) else {
                    return false;
                };
                assert_eq!(frames[index]["ok"], false);
                assert_eq!(frames[index]["error"], expected_error);
                assert_eq!(frames[index]["hub"], "local");
                let subsequent = &frames[index + 1..];
                for op in ["settings", "state"] {
                    let Some(refresh) = subsequent.iter().find(|v| v["op"] == op) else {
                        return false;
                    };
                    let channel = refresh["settings"]["channels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|ch| ch["id"] == "channel")
                        .unwrap();
                    assert_eq!(channel["configured"], true);
                    assert_eq!(channel["status"], "ready");
                }
                true
            });
        }
    }
}

#[test]
fn user_plugin_overrides_take_precedence_and_fall_back_after_removal() {
    if std::env::var_os("CITO_TEST_PLUGIN_PRIORITY").is_some() {
        let (config, _) = cito_native::plugins::load_native_config().unwrap();
        let dirs = [
            config.home_dir.join("first/pi"),
            config.home_dir.join("second/pi"),
            config.state_dir.join("plugins/pi"),
        ];
        for (index, expected) in [
            "First override",
            "Second override",
            "User override",
            "builtin",
        ]
        .into_iter()
        .enumerate()
        {
            let (_, plugins) = cito_native::plugins::load_native_config().unwrap();
            let plugin = plugins.find_by_id("pi").unwrap();
            assert_eq!(plugins.plugins.iter().filter(|p| p.id == "pi").count(), 1);
            assert_eq!(plugin.is_builtin, index == 3);
            cito_native::setup::ensure_cli_shims_native(&config.state_dir, &plugins).unwrap();
            if index < 3 {
                assert_eq!(plugin.display_name, expected);
                assert_eq!(plugin.plugin_dir, dirs[index]);
                let actual =
                    cito_native::plugins::plugin_request(plugin, &json!({"op":"probe"})).unwrap();
                assert_eq!(actual["source"], expected);
                assert_eq!(
                    actual["cwd"],
                    json!(fs::canonicalize(&dirs[index]).unwrap())
                );
                assert_eq!(
                    cito_native::setup::inspect_plugin_setup(plugin),
                    (true, true, vec![dirs[index].display().to_string()])
                );
                assert!(!config.state_dir.join("runtime/plugins/pi").exists());
                let manifest: Value = serde_json::from_slice(
                    &fs::read(config.state_dir.join("plugins/pi/plugin.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    manifest["displayName"], "User override",
                    "Staging cannot overwrite a user plugin"
                );
                fs::remove_dir_all(&dirs[index]).unwrap();
            } else {
                assert_eq!(plugin.display_name, "Pi Coding Agent");
                assert!(config
                    .state_dir
                    .join("runtime/plugins/pi/plugin.json")
                    .is_file());
                assert!(!config.state_dir.join("plugins/pi").exists());
                assert!(cito_native::setup::is_staged_plugin_intact(
                    &config.state_dir,
                    plugin
                ));
            }
        }
        return;
    }
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let driver = f.dir.join("override.py");
    script(&driver, "# purpose: Report the actual selected plugin command and setup source.\n# usage: python3 override.py DISPLAY_NAME\n# effects: Reads one request; emits JSON only.\n# requires: Python standard library.\nimport json, os, sys\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'ok':True,'installed':True,'configured':True,'source':sys.argv[1],'cwd':os.getcwd(),'paths':[r.get('plugin_dir','')]}))\n");
    for (dir, display) in [
        (f.home.join("first/pi"), "First override"),
        (f.home.join("second/pi"), "Second override"),
        (f.state.join("plugins/pi"), "User override"),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plugin.json"), json!({"id":"pi","kind":"agent","displayName":display,"command":["python3",driver,display]}).to_string()).unwrap();
    }
    f.env
        .insert("CITO_PLUGIN_PATH".into(), " ~/first : ~/second ".into());
    f.env.insert("CITO_TEST_PLUGIN_PRIORITY".into(), "1".into());
    let (ok, out, err) = f.command(
        std::env::current_exe().unwrap().to_str().unwrap(),
        &[
            "--exact",
            "daemon::user_plugin_overrides_take_precedence_and_fall_back_after_removal",
            "--nocapture",
        ],
        "",
    );
    assert!(ok, "{out}\n{err}");
}

#[test]
fn legacy_cleanup_rejects_invalid_native_config_without_deleting_plugins() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    f.env.remove("CITO_USER");
    let legacy = f.state.join("plugins/pi");
    fs::create_dir_all(&legacy).unwrap();
    let manifest = json!({"id":"pi","kind":"agent","displayName":"Legacy copy",
        "version":"0.20.3","command":["./bin/cito-plugin-pi"]})
    .to_string();
    fs::write(legacy.join("plugin.json"), &manifest).unwrap();
    for raw in [
        "{",
        "[]",
        "{\"daemonPort\":0}",
        "{\"user\":\"invalid user\"}",
        "{\"hubs\":{\"work\":{\"url\":\"invalid\"}}}",
    ] {
        fs::write(&f.config, raw).unwrap();
        let (ok, out, err) = f.command(NATIVE, &["daemon"], "");
        assert!(!ok, "{raw}: {out} {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(fs::read_to_string(&f.config).unwrap(), raw);
    }
    fs::write(&f.config, "{}").unwrap();
    f.env.insert("CITO_DAEMON_PORT".into(), "invalid".into());
    let (ok, out, err) = f.command(NATIVE, &["daemon"], "");
    assert!(!ok, "{out} {err}");
    assert!(err.contains("CITO_DAEMON_PORT"), "{err}");
    assert_eq!(
        fs::read_to_string(legacy.join("plugin.json")).unwrap(),
        manifest
    );
}

#[test]
fn bundled_plugin_sources_survive_user_root_and_child_symlinks() {
    if std::env::var_os("CITO_TEST_SOURCE_ALIASES").is_some() {
        use cito_native::{core::hub_plugins, plugins, setup};
        let state = std::path::PathBuf::from(std::env::var("CITO_STATE_DIR").unwrap());
        let source = std::path::PathBuf::from(std::env::var("CITO_BUILTIN_PLUGINS_DIR").unwrap());
        let user = state.join("plugins");
        assert_eq!(
            setup::resolve_bundled_plugin_dir("human"),
            source.join("human")
        );
        let originals: Vec<_> = ["human", "telegram", "pi"]
            .into_iter()
            .map(|id| (id, fs::read(source.join(id).join("plugin.json")).unwrap()))
            .collect();
        for parent_alias in [true, false] {
            for native in [true, false] {
                if parent_alias {
                    std::os::unix::fs::symlink(&source, &user).unwrap();
                } else {
                    fs::create_dir_all(&user).unwrap();
                    for (id, _) in &originals {
                        std::os::unix::fs::symlink(source.join(id), user.join(id)).unwrap();
                    }
                }
                let readonly = plugins::load_plugins_readonly(&state);
                assert!(readonly.find_by_id("human").unwrap().is_builtin);
                assert!(user.join("human").exists());
                if native {
                    let loaded = plugins::load_plugins(&state);
                    for (id, _) in &originals {
                        let plugin = loaded.find_by_id(id).unwrap();
                        assert!(plugin.is_builtin, "{id}");
                        assert_eq!(plugin.plugin_dir, source.join(id));
                    }
                } else {
                    let channels =
                        hub_plugins::discover(&[user.clone(), source.clone()], Some(&user))
                            .unwrap();
                    assert_eq!(
                        channels.iter().find(|p| p.id == "telegram").unwrap().dir,
                        source.join("telegram")
                    );
                }
                for (id, original) in &originals {
                    assert_eq!(
                        fs::read(source.join(id).join("plugin.json")).unwrap(),
                        *original
                    );
                }
                if parent_alias {
                    assert!(user.is_symlink());
                    fs::remove_file(&user).unwrap();
                } else {
                    assert!(fs::read_dir(&user).unwrap().next().is_none());
                    fs::remove_dir(&user).unwrap();
                }
            }
        }
        return;
    }
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    // Relocate our own libtest binary so the real packaged-root resolver uses sandbox data.
    // No system binary, desktop application, daemon, or network service is launched.
    let package = f.dir.join("package");
    let executable = package.join("bin/plugin-test");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let source = package.join("plugins");
    for id in ["human", "telegram", "pi"] {
        fs::create_dir_all(source.join(id)).unwrap();
        fs::copy(
            root().join("plugins").join(id).join("plugin.json"),
            source.join(id).join("plugin.json"),
        )
        .unwrap();
    }
    f.env.insert(
        "CITO_BUILTIN_PLUGINS_DIR".into(),
        source.display().to_string(),
    );
    f.env.insert("CITO_TEST_SOURCE_ALIASES".into(), "1".into());
    let (ok, out, err) = f.command(
        executable.to_str().unwrap(),
        &[
            "--exact",
            "daemon::bundled_plugin_sources_survive_user_root_and_child_symlinks",
            "--nocapture",
        ],
        "",
    );
    assert!(ok, "{out} {err}");
}

#[test]
fn legacy_staged_plugins_migrate_without_discarding_custom_overrides() {
    if let Ok(mode) = std::env::var("CITO_TEST_LEGACY_PLUGINS") {
        use cito_native::{core::hub_plugins, plugins};
        let state = std::path::PathBuf::from(std::env::var("CITO_STATE_DIR").unwrap());
        let user_root = state.join("plugins");
        let ids = ["pi", "codex", "claude", "telegram", "human"];
        if mode == "native" {
            let config_path = std::path::PathBuf::from(std::env::var("CITO_CONFIG_PATH").unwrap());
            let original_config = fs::read(&config_path).unwrap();
            let legacy_manifest = fs::read(user_root.join("pi/plugin.json")).unwrap();
            for hub in ["local", "work"] {
                for user in [
                    json!("invalid user"),
                    json!("self"),
                    json!(""),
                    json!(" "),
                    json!(" alice "),
                    json!(42),
                    json!(false),
                    json!([]),
                    json!({}),
                ] {
                    let mut raw =
                        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}});
                    raw["hubs"][hub]["user"] = user;
                    fs::write(&config_path, raw.to_string()).unwrap();
                    assert_eq!(
                        plugins::load_native_config().unwrap_err(),
                        format!("Invalid user for hub {hub}")
                    );
                    assert_eq!(
                        fs::read(user_root.join("pi/plugin.json")).unwrap(),
                        legacy_manifest
                    );
                    for id in ids {
                        assert!(user_root.join(id).is_dir(), "rejected config removed {id}");
                    }
                }
            }
            fs::write(
                &config_path,
                json!({"hubs":{"local":{"url":"local://","user":null}}}).to_string(),
            )
            .unwrap();
            let (config, loaded) = plugins::load_native_config().unwrap();
            assert_eq!(config.effective_user("local").unwrap(), config.user);
            fs::write(config_path, original_config).unwrap();
            for id in ids {
                let plugin = loaded.find_by_id(id).unwrap();
                assert!(plugin.is_builtin, "{id}");
                let manifest: Value = serde_json::from_slice(
                    &fs::read(plugin.plugin_dir.join("plugin.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(manifest["version"], plugins::CITO_VERSION);
            }
        } else {
            let loaded =
                hub_plugins::discover(&hub_plugins::default_roots(&state), Some(&user_root))
                    .unwrap();
            let channel = loaded.iter().find(|p| p.id == "telegram").unwrap();
            assert_eq!(channel.manifest["version"], plugins::CITO_VERSION);
            assert!(channel.dir.ends_with("plugins/telegram"));
            assert_ne!(channel.dir, user_root.join("telegram"));
            assert!(!loaded.iter().any(|p| p.id == "human"));
        }
        for id in ids {
            assert!(!user_root.join(id).exists(), "legacy copy remains: {id}");
        }
        let external = std::path::PathBuf::from(std::env::var("CITO_PLUGIN_DIR").unwrap());
        assert!(external.join("human/plugin.json").is_file());
        assert!(external.join("alias/plugin.json").is_file());
        // The companion stays reserved even for a current-version custom command.
        let companion = user_root.join("human");
        fs::create_dir_all(&companion).unwrap();
        fs::write(companion.join("plugin.json"), json!({"id":"human","kind":"human",
            "displayName":"Custom companion","version":plugins::CITO_VERSION,"command":["./run.sh"]}).to_string()).unwrap();
        let loaded = plugins::load_plugins(&state);
        assert!(loaded.find_by_id("human").unwrap().is_builtin);
        assert!(!companion.exists());
        // Custom commands survive any version; native-shaped commands survive at the current version.
        for (version, command) in [
            (Value::Null, json!(["./run.sh"])),
            (json!("custom"), json!(["node", "custom.js"])),
            (
                json!(plugins::CITO_VERSION),
                json!(["./bin/cito-plugin-custom"]),
            ),
        ] {
            for (id, kind) in [("pi", "agent"), ("telegram", "human-channel")] {
                let dir = user_root.join(id);
                fs::create_dir_all(&dir).unwrap();
                fs::write(
                    dir.join("plugin.json"),
                    json!({"id":id,"kind":kind,
                    "displayName":"Custom override","version":version,"command":command})
                    .to_string(),
                )
                .unwrap();
            }
            let loaded = plugins::load_plugins(&state);
            for id in ["pi", "telegram"] {
                let plugin = loaded.find_by_id(id).unwrap();
                assert!(!plugin.is_builtin);
                assert_eq!(plugin.plugin_dir, user_root.join(id));
            }
            let channels =
                hub_plugins::discover(&hub_plugins::default_roots(&state), Some(&user_root))
                    .unwrap();
            assert_eq!(
                channels.iter().find(|p| p.id == "telegram").unwrap().dir,
                user_root.join("telegram")
            );
        }
        return;
    }
    let _lock = serial();
    for mode in ["native", "hub"] {
        let mut f = Fixture::new(json!({}));
        for id in ["pi", "codex", "claude", "telegram", "human"] {
            let dir = f.state.join("plugins").join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("plugin.json"), json!({"id":id,"kind":if id == "human" || id == "telegram" {"human-channel"} else {"agent"},
                "displayName":"Legacy staged copy","version":"0.20.3",
                "command":[format!("./bin/cito-plugin-{id}")]}).to_string()).unwrap();
        }
        let external = f.dir.join("external");
        for name in ["human", "alias"] {
            let dir = external.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("plugin.json"),
                json!({"id":"human","kind":"human-channel",
                "displayName":"External companion","command":["./run.sh"]})
                .to_string(),
            )
            .unwrap();
        }
        f.env
            .insert("CITO_PLUGIN_PATH".into(), external.display().to_string());
        f.env
            .insert("CITO_PLUGIN_DIR".into(), external.display().to_string());
        f.env.insert("CITO_TEST_LEGACY_PLUGINS".into(), mode.into());
        let (ok, out, err) = f.command(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &[
                "--exact",
                "daemon::legacy_staged_plugins_migrate_without_discarding_custom_overrides",
                "--nocapture",
            ],
            "",
        );
        assert!(ok, "{mode}: {out}\n{err}");
    }
}

#[test]
fn lifecycle_setup_repairs_deleted_builtin_loaded_from_staged_plugins() {
    if std::env::var_os("CITO_TEST_STAGED_PLUGINS").is_some() {
        // Re-enter under the fixture environment without changing process-global
        // variables in the parent test runner.
        let (config, plugins) = cito_native::plugins::load_native_config().unwrap();
        let custom = plugins.find_by_id("custom-agent").unwrap();
        let custom_dir = config.state_dir.join("plugins/custom-agent");
        let cwd = fs::canonicalize(&custom_dir).unwrap();
        assert!(!custom.is_builtin);
        assert_eq!(custom.plugin_dir, custom_dir);
        let response =
            cito_native::plugins::plugin_request(custom, &json!({"op":"setup_inspect"})).unwrap();
        assert_eq!(response["cwd"], json!(cwd));
        assert_eq!(response["executable"], json!(custom_dir.join("./run.sh")));
        assert_eq!(
            cito_native::setup::inspect_plugin_setup(custom),
            (true, true, vec![cwd.display().to_string()])
        );
        assert!(plugins.find_by_id("pi").unwrap().is_builtin);
        return;
    }

    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
    f.stop(daemon);
    let custom = f.state.join("plugins/custom-agent");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        custom.join("plugin.json"),
        json!({"id":"custom-agent","kind":"agent","displayName":"Custom Agent","command":["./run.sh"]}).to_string(),
    ).unwrap();
    script(
        &custom.join("run.sh"),
        "#!/bin/sh\nread -r request\nprintf '{\"ok\":true,\"installed\":true,\"configured\":true,\"paths\":[\"%s\"],\"cwd\":\"%s\",\"executable\":\"%s\"}\\n' \"$(pwd -P)\" \"$(pwd -P)\" \"$0\"\n",
    );
    let check_plugins = |f: &mut Fixture| {
        f.env.insert("CITO_TEST_STAGED_PLUGINS".into(), "1".into());
        let (ok, out, err) = f.command(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &[
                "--exact",
                "daemon::lifecycle_setup_repairs_deleted_builtin_loaded_from_staged_plugins",
                "--nocapture",
            ],
            "",
        );
        f.env.remove("CITO_TEST_STAGED_PLUGINS");
        assert!(ok, "{out}\n{err}");
    };
    f.env.insert(
        "CITO_BUILTIN_PLUGINS_DIR".into(),
        f.state.join("runtime/plugins").display().to_string(),
    );
    check_plugins(&mut f);
    let mut daemon = f.daemon(&[]);
    let app = |f: &Fixture, id: &str| {
        let snapshot = f.action(json!({"op":"get_snapshot"})).1;
        let apps: Vec<_> = snapshot["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["id"] == id)
            .collect();
        assert_eq!(apps.len(), 1, "descriptor must remain unique: {id}");
        apps[0].clone()
    };
    assert_eq!(app(&f, "pi")["status"], "ready");
    assert_eq!(app(&f, "custom-agent")["status"], "ready");
    // Repairs must use the distribution even though the loaded descriptor is staged.
    for asset in [
        "cito-bridge.ts",
        "package.json",
        "plugin.json",
        "bin/cito-plugin-pi",
    ] {
        let staged = f.state.join("runtime/plugins/pi").join(asset);
        fs::remove_file(&staged).unwrap();
        assert_eq!(app(&f, "pi")["status"], "repair_needed", "{asset}");
        assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
        assert_eq!(
            fs::read(&staged).unwrap(),
            fs::read(root().join("plugins/pi").join(asset)).unwrap()
        );
        assert_eq!(app(&f, "pi")["status"], "ready", "{asset}");
    }
    let alias = f.dir.join("staged-plugins");
    std::os::unix::fs::symlink(f.state.join("runtime/plugins"), &alias).unwrap();
    for builtin_root in [
        f.state.join("runtime/plugins"),
        alias,
        f.state.join("plugins"),
    ] {
        f.env.insert(
            "CITO_BUILTIN_PLUGINS_DIR".into(),
            builtin_root.display().to_string(),
        );
        for missing_manifest in [true, false] {
            f.stop(daemon);
            let staged = f.state.join("runtime/plugins/pi");
            if missing_manifest {
                fs::remove_file(staged.join("plugin.json")).unwrap();
            } else {
                fs::remove_dir_all(&staged).unwrap();
            }
            // A new process must rediscover the descriptor, including through an alias.
            check_plugins(&mut f);
            daemon = f.daemon(&[]);
            assert_eq!(app(&f, "pi")["status"], "repair_needed");
            let custom_app = app(&f, "custom-agent");
            assert_eq!(custom_app["status"], "ready");
            assert_eq!(
                custom_app["paths"],
                json!([fs::canonicalize(&custom).unwrap()])
            );
            assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
            for asset in ["plugin.json", "bin/cito-plugin-pi", "cito-bridge.ts"] {
                assert_eq!(
                    fs::read(staged.join(asset)).unwrap(),
                    fs::read(root().join("plugins/pi").join(asset)).unwrap()
                );
            }
            assert_eq!(app(&f, "pi")["status"], "ready");
            assert_eq!(app(&f, "pi")["configured"], true);
        }
    }
}

#[test]
fn lifecycle_remote_only_channel_schema_and_selected_settings_hub() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let plugin = host.state.join("hub/plugins/remote-channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"),json!({"id":"remote-channel","kind":"human-channel","displayName":"Remote channel","scope":"hub",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"setupGuide":["Configure service, then pair."],"pairing":{"supported":true},
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let (_, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"shared":{"url":format!("http://127.0.0.1:{port}")}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    assert!(!f.state.join("plugins/remote-channel").exists());
    let (status,result)=f.action(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"credential":"saved","address":"room"}}));
    assert_eq!(status, 200, "{result}");
    let db = cito_native::database::Database::open(&host.state.join("hub/hub.sqlite")).unwrap();
    let before = db.query("SELECT * FROM hub_plugin_services", &[]).unwrap();
    assert_eq!(f.action(json!({"op":"channel_test","hub":"shared","channel_id":"remote-channel","values":{"credential":"candidate","address":"test-room"}})).0,200);
    assert_eq!(
        db.query("SELECT * FROM hub_plugin_services", &[]).unwrap(),
        before
    );
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["settings"]["settings_hub"], "shared");
    let ch = &snapshot["settings"]["channels"][0];
    assert_eq!(ch["status"], "ready");
    assert_eq!(ch["fields"][0]["hidden"], false);
    assert_eq!(ch["setup_guide"], json!(["Configure service, then pair."]));
    assert_eq!(f.action(json!({"op":"channel_toggle","hub":"shared","channel_id":"remote-channel","enabled":false})).1["channels"][0]["status"],"paused");
    assert_eq!(
        f.action(json!({"op":"channel_pair","hub":"shared","channel_id":"remote-channel"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"channel_cancel_pair","hub":"shared","channel_id":"remote-channel"}))
            .0,
        200
    );
    assert!(db
        .query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[]
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"channel_unpair","hub":"shared","channel_id":"remote-channel"}))
            .1["channels"][0]["status"],
        "hub_ready"
    );
    let paired =
        f.action(json!({"op":"channel_pair","hub":"shared","channel_id":"remote-channel"}));
    assert_eq!(paired.0, 200, "{paired:?}");
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"],
        "pairing_pending"
    );
    let code = db
        .query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[],
        )
        .unwrap()[0]["code"]
        .clone();
    // The plugin redeems on the remote Hub, outside the daemon's action path.
    rpc(
        port,
        "human_channels_test",
        json!({"plugin_id":"remote-channel","user_config":{"address":format!("redeem:{}", code.as_str().unwrap())}}),
    );
    wait(|| {
        db.query(
            "SELECT code FROM hub_pairing_codes WHERE consumed_ms IS NULL",
            &[],
        )
        .unwrap()
        .is_empty()
    });
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "ready"
    });
    assert_eq!(f.action(json!({"op":"disconnect_human_channel","hub":"shared","channel_id":"remote-channel","reset_hub":true})).1["channels"][0]["status"],"not_configured");
}
#[test]
fn selected_remote_channel_refresh_is_nonblocking_and_recovers_invalidated_slots() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let plugin = host.state.join("hub/plugins/remote-channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), json!({"id":"remote-channel","kind":"human-channel","displayName":"Remote channel","scope":"hub",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let (_, hub_port) = host.hub();
    let proxy_port = port();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"shared":{"url":format!("http://127.0.0.1:{proxy_port}"),"user":"alice"}},"default_hub":"local"}),
    );
    let proxy_script = f.dir.join("inventory-proxy.py");
    script(
        &proxy_script,
        r#"#!/usr/bin/env python3
# purpose: Hold real inventory responses until the test releases each request.
# usage: python3 inventory-proxy.py LISTEN_PORT HUB_PORT
# effects: Loopback HTTP only; Fixture owns and reaps the process.
# requires: Python standard library and the sandbox Hub.
import http.server, json, socketserver, sys, threading, urllib.request
lock = threading.Lock()
hold = False
pending = []
completed = []
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, body):
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try: self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError): pass
    def do_GET(self):
        global hold
        with lock:
            if self.path == '/hold': hold = True
            if self.path.startswith('/release/'):
                pending[int(self.path.rsplit('/', 1)[1]) - 1].set()
            body = json.dumps({'waiting': len(pending), 'completed': completed}).encode()
        self.reply(body)
    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        request = urllib.request.Request('http://127.0.0.1:' + sys.argv[2] + '/v1/rpc', data=body,
            headers={k: v for k, v in self.headers.items() if k.lower() in ('authorization', 'x-cito-user', 'content-type')})
        with urllib.request.urlopen(request, timeout=2) as response: result = response.read()
        gate = None
        if json.loads(body)['op'] == 'human_channels_list':
            with lock:
                gate = threading.Event() if hold else None
                if gate: pending.append(gate)
            if gate and not gate.wait(10): return
        self.reply(result)
        if gate:
            with lock: completed.append(pending.index(gate) + 1)
socketserver.ThreadingTCPServer.allow_reuse_address = True
server = socketserver.ThreadingTCPServer(('127.0.0.1', int(sys.argv[1])), Handler)
print(json.dumps({'op': 'ready'}), flush=True)
server.serve_forever()
"#,
    );
    let proxy = f.spawn(
        "python3",
        &[
            proxy_script.to_str().unwrap(),
            &proxy_port.to_string(),
            &hub_port.to_string(),
        ],
        &[],
    );
    wait(|| f.frames(proxy).iter().any(|frame| frame["op"] == "ready"));
    f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"credential":"saved","address":"room"}})).1["channels"][0]["status"], "ready");
    let query = "  remote-mac-42  ";
    let (status, response) = f.action(
        json!({"op":"query_directory","mode":"activity","query":query,"request_id":"padded-query"}),
    );
    assert_eq!(status, 200);
    assert_eq!(response["query"], query);
    assert_eq!(response["request_id"], "padded-query");
    assert_eq!(http(proxy_port, "/hold", None).0, 200);
    let mut config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    config["hubs"]["shared"]["token"] = json!("rotated-credential");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| http(proxy_port, "/status", None).1["waiting"] == 1);
    // The server has accepted the RPC and holds its response until explicit release.
    let local = |path: &str| -> Value {
        ureq::get(&format!("http://127.0.0.1:{}{path}", f.port))
            .timeout(std::time::Duration::from_millis(750))
            .call()
            .unwrap()
            .into_json()
            .unwrap()
    };
    assert_eq!(local("/health")["ok"], true);
    let daemon_port = f.port;
    for op in ["open_settings", "settings_select_hub"] {
        let opened = std::thread::spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_millis(750))
                .send_json(json!({"op":op,"hub":"shared"}))
                .and_then(|response| response.into_json::<Value>().map_err(Into::into))
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(opened["ok"], true);
        assert_eq!(
            http(proxy_port, "/status", None).1["waiting"],
            1,
            "{op} must share the held inventory request"
        );
        assert_eq!(local("/health")["ok"], true);
    }
    let snapshot: Value = ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
        .timeout(std::time::Duration::from_millis(750))
        .send_json(json!({"op":"get_snapshot"}))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(snapshot["settings"]["settings_hub"], "shared");
    assert_eq!(
        snapshot["settings"]["channels"],
        json!([]),
        "Invalidated inventory must be loading"
    );
    // Replace the slot again while Alice's response is in flight. Bob must never
    // receive Alice's configured personal binding, including after its late reply.
    config["hubs"]["shared"]["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| http(proxy_port, "/status", None).1["waiting"] == 2);
    assert_eq!(local("/health")["ok"], true);
    assert_eq!(http(proxy_port, "/release/2", None).0, 200);
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "hub_ready"
    });
    assert_eq!(http(proxy_port, "/release/1", None).0, 200);
    wait(|| {
        http(proxy_port, "/status", None).1["completed"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(1)))
    });
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["settings"]["channels"][0]["configured"], false);
    assert_eq!(snapshot["settings"]["channels"][0]["status"], "hub_ready");

    // A mutation queued behind an old inventory must await its own schema and
    // post-mutation inventory before acknowledging, without retaining service.
    config["hubs"]["shared"]["token"] = json!("next-credential");
    fs::write(&f.config, config.to_string()).unwrap();
    wait(|| http(proxy_port, "/status", None).1["waiting"] == 3);
    std::thread::scope(|scope| {
        let saved = scope.spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_secs(8))
                .send_json(json!({"op":"channel_save","hub":"shared","channel_id":"remote-channel","enabled":true,"values":{"address":"bob-room"}}))
                .and_then(|response| response.into_json::<Value>().map_err(Into::into))
        });
        assert_eq!(local("/health")["ok"], true);
        assert_eq!(http(proxy_port, "/status", None).1["waiting"], 3);
        assert_eq!(http(proxy_port, "/release/3", None).0, 200);
        wait(|| http(proxy_port, "/status", None).1["waiting"] == 4);
        assert_eq!(
            local("/health")["ok"],
            true,
            "Schema lookup must release service"
        );
        assert_eq!(http(proxy_port, "/release/4", None).0, 200);
        wait(|| http(proxy_port, "/status", None).1["waiting"] == 5);
        assert!(
            !saved.is_finished(),
            "The acknowledgement must await post-mutation inventory"
        );
        assert_eq!(
            local("/health")["ok"],
            true,
            "Post-mutation inventory must release service"
        );
        assert_eq!(
            f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["configured"],
            false
        );
        assert_eq!(http(proxy_port, "/release/5", None).0, 200);
        let result = saved.join().unwrap().unwrap();
        assert_eq!(result["channels"][0]["status"], "ready");
    });
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(snapshot["settings"]["channels"][0]["status"], "ready");
    assert_eq!(snapshot["settings"]["channels"][0]["configured"], true);

    // A replaced slot cannot acknowledge an in-flight mutation against the new
    // identity, even when its remote mutation already committed before replacement.
    std::thread::scope(|scope| {
        let changed = scope.spawn(move || {
            ureq::post(&format!("http://127.0.0.1:{daemon_port}/ui"))
                .timeout(std::time::Duration::from_secs(8))
                .send_json(
                    json!({"op":"channel_unpair","hub":"shared","channel_id":"remote-channel"}),
                )
        });
        wait(|| http(proxy_port, "/status", None).1["waiting"] == 6);
        config["hubs"]["shared"]["token"] = json!("replacement-credential");
        fs::write(&f.config, config.to_string()).unwrap();
        wait(|| http(proxy_port, "/status", None).1["waiting"] == 7);
        let failure = changed.join().unwrap().unwrap_err();
        match failure {
            ureq::Error::Status(400, response) => assert!(response
                .into_string()
                .unwrap()
                .contains("Hub changed during channel request")),
            other => panic!("Unexpected mutation failure: {other}"),
        }
    });
    assert_eq!(local("/health")["ok"], true);
    assert_eq!(http(proxy_port, "/release/7", None).0, 200);
    wait(|| {
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["status"] == "hub_ready"
    });
    assert_eq!(http(proxy_port, "/release/6", None).0, 200);
    wait(|| {
        http(proxy_port, "/status", None).1["completed"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(6)))
    });
    assert_eq!(
        f.action(json!({"op":"get_snapshot"})).1["settings"]["channels"][0]["configured"],
        false
    );
    config["hubs"]["shared"]["enabled"] = json!(false);
    fs::write(&f.config, config.to_string()).unwrap();
    for op in ["open_settings", "settings_select_hub"] {
        assert_eq!(
            f.action(json!({"op":op,"hub":"shared"})).0,
            200,
            "Disabled hubs must remain configurable"
        );
    }
}

#[test]
fn lifecycle_setup_repairs_shims_disconnects_listeners_and_keeps_daemon_ui_alive() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let ui = f.recording_ui();
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    wait(|| f.home.join("ui-pid").exists());
    enroll(&f, meta.clone());
    assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
    let app = |snapshot: Value| {
        snapshot["settings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "pi")
            .unwrap()
            .clone()
    };
    assert_eq!(
        app(f.action(json!({"op":"get_snapshot"})).1)["status"],
        "ready"
    );
    fs::remove_dir_all(f.state.join("bin")).unwrap();
    fs::remove_dir_all(f.state.join("runtime/plugins")).unwrap();
    assert!(f.state.join("plugins/fixture/plugin.json").is_file());
    assert_eq!(
        app(f.action(json!({"op":"get_snapshot"})).1)["status"],
        "repair_needed"
    );
    assert_eq!(f.action(json!({"op":"setup_apply","harness":"pi"})).0, 200);
    assert!(f.state.join("bin/cito-mcp").is_file());
    assert!(f.state.join("runtime/plugins/pi/plugin.json").is_file());
    assert!(app(f.action(json!({"op":"get_snapshot"})).1)["detail"]
        .as_str()
        .unwrap()
        .contains("/reload"));
    enroll(&f, meta.clone());
    assert_eq!(
        f.action(json!({"op":"setup_remove_app","harness":"fixture"}))
            .0,
        200
    );
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    enroll(&f, meta);
    assert_eq!(f.action(json!({"op":"setup_disconnect_all"})).0, 200);
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE status='ACTIVE'")
        .is_empty());
    assert!(!f.exited(daemon));
    let ui_pid = fs::read_to_string(f.home.join("ui-pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(cito_native::plugins::pid_alive(ui_pid));
    assert_eq!(http(f.port, "/health", None).0, 200);
    assert!(f.state.join("bin/cito-mcp").is_file());
    assert!(f.state.join("runtime/plugins/pi").is_dir());
    let host: Value =
        serde_json::from_str(&fs::read_to_string(f.home.join(".pi/agent/settings.json")).unwrap())
            .unwrap();
    assert!(
        host["packages"].is_null() || host["packages"] == json!([]),
        "{host}"
    );
}
#[test]
fn lifecycle_sign_in_enables_disabled_hub_before_bounded_discovery_failure() {
    let _lock = serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let server = std::thread::spawn(move || {
        use std::io::Write;
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            } else {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    });
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{port}"),"enabled":false,"authMode":"oidc","issuer":format!("http://127.0.0.1:{port}"),"clientId":"client"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    let result = f.action(json!({"op":"login_hub","hub":"work"}));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(result.0, 400, "{result:?}");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(raw["hubs"]["work"]["enabled"], true);
    assert!(
        !f.home.join("mock-calls").exists(),
        "discovery failure must never open a browser"
    );
}
#[test]
fn lifecycle_hub_edit_preserves_secrets_disabled_state_default_and_host_error_rollback() {
    let _lock = serial();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":"http://127.0.0.1:1","authMode":"token","token":"saved","enabled":false,"clientSecret":"retained"}},"default_hub":"local"}),
    );
    f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"setup_save_hub","edit":true,"original_name":"work","name":"renamed","url":"http://127.0.0.1:2","auth_mode":"token","token":"","clientSecret":""})).0,200);
    let raw: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(raw["default_hub"], "local");
    assert!(raw["hubs"].get("work").is_none());
    assert_eq!(raw["hubs"]["renamed"]["token"], "saved");
    assert_eq!(raw["hubs"]["renamed"]["clientSecret"], "retained");
    assert_eq!(raw["hubs"]["renamed"]["enabled"], false);
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    let h = snapshot["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "renamed")
        .unwrap();
    assert_eq!(h["status"], "disconnected");
    assert_eq!(h["connected"], false);
    assert_eq!(f.action(json!({"op":"set_host_hub","enabled":true,"port":0,"bind":"127.0.0.1","auth_mode":"none"})).0,200);
    let running = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    let blocked = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocked.local_addr().unwrap().port();
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":port}))
            .0,
        400
    );
    let failed = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert_eq!(failed["port"], running["port"]);
    assert!(failed["error"].is_string());
    assert_eq!(failed["running"], true);
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":true,"port":0,"auth_mode":"token"}))
            .0,
        200
    );
    let success = f.action(json!({"op":"get_snapshot"})).1["settings"]["host_hub"].clone();
    assert!(success["error"].is_null());
    assert_eq!(success["auth_mode"], "token");
}
#[test]
fn inbox_ordered_pages_explicit_ack_union_and_no_background_wake() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    enroll(&f, meta.clone());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let db = f.db("state.sqlite");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    db.execute(
        "INSERT OR REPLACE INTO wake_retries(hub,listener_id,retry_at_ms) VALUES('local',?,?)",
        &[row["listener_id"].clone(), json!(now + 60_000)],
    )
    .unwrap();
    db.transaction(|c|{for i in (1..=55).rev(){c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,?,?,?,'bob','laptop','Sender','Message','2030-01-01T00:00:00.000Z',?,'RECEIVED')",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!(now+60_000)])?;}Ok(())}).unwrap();
    let selected = f.tool("cito_inbox", json!({"ack_ids":["sig-55"]}), meta.clone());
    assert_eq!(selected["acknowledged_count"], 1);
    assert_eq!(
        selected["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=50).collect::<Vec<_>>()
    );
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["acknowledged_count"],
        50
    );
    let union = f.tool(
        "cito_inbox",
        json!({"ack_ids":["sig-55"],"ack_all":true}),
        meta.clone(),
    );
    assert_eq!(union["acknowledged_count"], 4);
    assert_eq!(union["signals"].as_array().unwrap().len(), 4);
    assert_eq!(f.tool("cito_inbox", json!({}), meta)["signals"], json!([]));
    assert_eq!(
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len(),
        55
    );
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(!calls
        .lines()
        .any(|l| serde_json::from_str::<Value>(l).unwrap()["op"] == "wake"));
}
#[test]
fn pending_publish_constraint_failure_recovers_exact_id_and_dedup_sequence() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let db = f.db("hub.sqlite");
    db.execute("CREATE TRIGGER reject_publish BEFORE INSERT ON hub_signals BEGIN SELECT RAISE(ABORT,'injected failure'); END",&[]).unwrap();
    let args = json!({"to":target["listener_id"],"message":"Recovery","idempotency_key":"same"});
    assert_eq!(
        f.tool_result("cito_send", args.clone(), human())["isError"],
        true
    );
    let row = f.rows("SELECT * FROM local_sent_idempotency")[0].clone();
    assert_eq!(row["status"], "PENDING_PUBLISH");
    db.execute("DROP TRIGGER reject_publish", &[]).unwrap();
    let recovered = f.tool("cito_send", args.clone(), human());
    assert_eq!(recovered["signal_id"], row["signal_id"]);
    assert_eq!(f.tool("cito_send", args, human())["seq"], recovered["seq"]);
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap()[0]["n"],
        1
    );
}
#[test]
fn shared_hub_private_scope_is_invisible_and_workspace_scope_is_deliverable() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config = json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"alice"}},"defaultHub":"work"});
    let a = Fixture::new(config);
    let meta = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let target = a.tool(
        "cito_listen",
        json!({"about":"Private target","scope":"self"}),
        meta.clone(),
    );
    let mut b = Fixture::new(
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}"),"user":"bob"}},"defaultHub":"work"}),
    );
    b.env.insert("CITO_USER".into(), "bob".into());
    b.daemon(&[]);
    assert_eq!(
        b.tool("cito_find", json!({"harness":"fixture"}), human())["listeners"],
        json!([])
    );
    let denied = b.tool_result(
        "cito_send",
        json!({"to":target["listener_id"],"message":"Private"}),
        human(),
    );
    assert_eq!(denied["isError"], true);
    assert!(denied.to_string().contains("FORBIDDEN_TARGET_SCOPE"));
    let public = a.tool(
        "cito_listen",
        json!({"about":"Workspace target","scope":"workspace"}),
        meta,
    );
    b.tool(
        "cito_send",
        json!({"to":public["listener_id"],"message":"Workspace"}),
        human(),
    );
    wait(|| {
        a.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
    });
    assert_eq!(
        a.rows("SELECT from_user FROM local_signals")[0]["from_user"],
        "bob"
    );
}
#[test]
fn same_channel_takeover_fences_old_owner_and_preserves_winner() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (_, p) = host.hub();
    let config =
        json!({"hubs":{"work":{"url":format!("http://127.0.0.1:{p}")}},"defaultHub":"work"});
    let a = Fixture::new(config.clone());
    let ma = a.provider("delivered_unconfirmed");
    a.daemon(&[]);
    let old = enroll(&a, ma);
    let sibling = Fixture::new(config.clone());
    let sibling_meta = sibling.provider("delivered_unconfirmed");
    sibling.daemon(&[]);
    let attached = enroll(&sibling, sibling_meta);
    assert_eq!(attached["binding_generation"], old["binding_generation"]);
    let sibling_row =
        sibling.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    // Hold only the sibling's dispatch lease; its receiver must keep polling unfenced.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    sibling.db("state.sqlite").execute(
        "UPDATE local_listeners SET dispatcher_token='test-held',lease_until_ms=? WHERE listener_id=?",
        &[json!(now + 60_000), sibling_row["listener_id"].clone()],
    ).unwrap();
    let b = Fixture::new(config);
    b.provider("delivered_unconfirmed");
    fs::write(
        b.state.join("host-id"),
        fs::read(a.state.join("host-id")).unwrap(),
    )
    .unwrap();
    b.daemon(&[]);
    let winner = enroll(
        &b,
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    // Heartbeat fencing must work even before another signal arrives.
    wait(|| {
        a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"]
            == "FENCED_INACTIVE"
    });
    let sent = b.tool(
        "cito_send",
        json!({"to":winner["listener_id"],"message":"Winner"}),
        human(),
    );
    wait(|| {
        b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
            .len()
            == 1
            && sibling.rows("SELECT * FROM local_signals").len() == 1
    });
    assert!(sibling.rows("SELECT held_reason FROM local_signals")[0]["held_reason"].is_null());
    assert_eq!(
        b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    assert_eq!(winner["binding_generation"], old["binding_generation"]);
    assert_eq!(
        sibling.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "ACTIVE"
    );
    let winner_row = b.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let old_row = a.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_ne!(winner_row["owner_token"], old_row["owner_token"]);
    let assert_channels_active = || {
        let remote: Value = serde_json::from_str(
            host.db("hub/hub.sqlite")
                .query(
                    "SELECT CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                    &[winner_row["listener_id"].clone()],
                )
                .unwrap()[0]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(remote["state"], "ACTIVE");
        for row in [&winner_row, &sibling_row] {
            let channel = format!("{}:fixture", row["host_id"].as_str().unwrap());
            assert_eq!(remote["channels"][&channel]["state"], "ACTIVE");
            assert_eq!(
                remote["channels"][&channel]["owner_token"],
                row["owner_token"]
            );
        }
    };
    assert_channels_active();
    let mut sent_signals = vec![sent];
    for operation in ["unlisten", "reload"] {
        if operation == "unlisten" {
            assert_eq!(
                a.tool(
                    "cito_unlisten",
                    json!({"hub":"work"}),
                    json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
                )["status"],
                "not_listening"
            );
            assert_eq!(
                a.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
                "UNLISTENED"
            );
        } else {
            // Recreate a reload that captured A's ACTIVE row before heartbeat fencing.
            // Defer background fencing so the reload must exercise remote detach.
            let db = a.db("state.sqlite");
            db.execute(
                "CREATE TRIGGER defer_fencing BEFORE UPDATE OF status ON local_listeners WHEN OLD.harness='fixture' AND NEW.status='FENCED_INACTIVE' BEGIN SELECT RAISE(IGNORE); END",
                &[],
            ).unwrap();
            db.execute(
                "UPDATE local_listeners SET status='ACTIVE' WHERE harness='fixture'",
                &[],
            )
            .unwrap();
            let mut config: Value = serde_json::from_slice(&fs::read(&a.config).unwrap()).unwrap();
            let url = format!("http://localhost:{p}");
            config["hubs"]["work"]["url"] = json!(url);
            fs::write(&a.config, config.to_string()).unwrap();
            let (status, snapshot) = a.action(json!({"op":"get_snapshot"}));
            // Finish the local fence deferred by the fixture's race injection.
            db.execute(
                "DROP TRIGGER defer_fencing; UPDATE local_listeners SET status='FENCED_INACTIVE' WHERE harness='fixture'",
                &[],
            ).unwrap();
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["settings"]["hubs"][0]["url"], url);
        }
        assert_channels_active();
        sent_signals.push(b.tool(
            "cito_send",
            json!({"to":winner["listener_id"],"message":format!("Winner after {operation}")}),
            human(),
        ));
        wait(|| {
            b.rows("SELECT * FROM local_signals WHERE state='DISPATCHED'")
                .len()
                == sent_signals.len()
        });
        assert_eq!(
            b.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
            "ACTIVE"
        );
    }
    for (fixture, expected) in [(&a, 0), (&b, sent_signals.len()), (&sibling, 0)] {
        let calls = fs::read_to_string(fixture.state.join("plugins/fixture/calls.jsonl")).unwrap();
        let wakes: Vec<Value> = calls
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["op"] == "wake")
            .collect();
        assert_eq!(wakes.len(), expected);
        for (wake, sent) in wakes.iter().zip(&sent_signals) {
            assert_eq!(
                wake["signals"][0]["signal_id"],
                sent["signal_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            );
        }
    }
}
#[test]
fn http_origin_guard_strict_tool_schemas_and_singleton_port_ownership() {
    let _lock = serial();
    let mut f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    let response = ureq::get(&format!("http://127.0.0.1:{}/health", f.port))
        .set("Origin", "https://foreign.example")
        .call();
    assert!(matches!(response, Err(ureq::Error::Status(403, _))));
    let tools = f.mcp("tools/list", json!({}));
    for tool in tools["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let name = tool["name"].as_str().unwrap();
        let result = f.tool_result(name, json!({"extra":true}), json!({}));
        assert_eq!(result["isError"], true);
        assert!(result.to_string().contains("INVALID_INPUT"));
    }
    assert!(f.command(NATIVE, &["daemon"], "").0);
    f.env.insert("CITO_DAEMON_PORT".into(), port().to_string());
    assert!(!f.command(NATIVE, &["daemon"], "").0);
    f.stop(daemon);
    f.env.insert("CITO_DAEMON_PORT".into(), f.port.to_string());
    f.daemon(&[]);
}
#[test]
fn sqlite_pre_v4_upgrade_and_v4_reopen_preserve_outbound_recovery_rows() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let db = f.db("state.sqlite");
    db.execute("CREATE TABLE local_listeners(legacy TEXT);CREATE TABLE local_pairwise_wakes(legacy TEXT);PRAGMA user_version=3",&[]).unwrap();
    let daemon = f.daemon(&[]);
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        4
    );
    assert!(db
        .query(
            "SELECT name FROM sqlite_master WHERE name='local_pairwise_wakes'",
            &[]
        )
        .unwrap()
        .is_empty());
    db.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES('work','d','p','s','c_abcdefgh2345','PENDING_PUBLISH','now',0)",&[]).unwrap();
    f.stop(daemon);
    f.daemon(&[]);
    assert_eq!(
        db.query(
            "SELECT signal_id FROM local_sent_idempotency WHERE hub='work' AND dedup_id='d'",
            &[]
        )
        .unwrap(),
        vec![json!({"signal_id":"s"})]
    );
}

#[test]
fn agent_and_human_sends_share_unicode_control_sanitization() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    let hub = f.db("hub.sqlite");
    let error = "INVALID_INPUT: message must contain 1..65,536 characters";
    let agent = f.tool_result(
        "cito_send",
        json!({"to":"@self","message":"\u{0081}"}),
        meta.clone(),
    );
    assert_eq!(agent["isError"], true);
    let agent_error: Value =
        serde_json::from_str(agent["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        agent_error,
        json!({"ok":false,"error":{"code":"INVALID_INPUT","message":"message must contain 1..65,536 characters","retryable":false}})
    );
    let (status, human) = f.action(
        json!({"op":"send_reply_from_human","to_listener_id":target["listener_id"],"text":"\u{0081}"}),
    );
    assert_eq!(status, 400);
    assert_eq!(human["error"], error);
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap(),
        vec![json!({"n":0})]
    );
    for (message, expected) in [
        ("a\u{0081}b", "ab"),
        (" \ta\u{0081}b\u{2003}", "ab"),
        (
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
            "a\n\r\t\u{000b}\u{000c}\u{0085}b",
        ),
    ] {
        let agent = f.tool(
            "cito_send",
            json!({"to":"@self","message":message}),
            meta.clone(),
        );
        let (status, human) = f.action(
            json!({"op":"send_reply_from_human","to_listener_id":target["listener_id"],"text":message}),
        );
        assert_eq!(status, 200, "{human}");
        for sent in [agent, human] {
            let id = sent["signal_id"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap();
            assert_eq!(
                hub.query(
                    "SELECT json_extract(CAST(data AS TEXT),'$.message') AS message FROM hub_signals WHERE signal_id=?",
                    &[json!(id)],
                ).unwrap(),
                vec![json!({"message":expected})]
            );
        }
    }
}

#[test]
fn human_desktop_replies_commit_publication_and_ack_atomically_then_reconcile_local_state() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"work":{"url":"local://"}},"defaultHub":"work"}));
    let meta = f.provider("delivered_unconfirmed");
    let channel = f.state.join("plugins/channel");
    fs::create_dir_all(&channel).unwrap();
    fs::write(channel.join("plugin.json"), json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "fields":[{"key":"credential","scope":"hub","type":"secret","required":true},{"key":"address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let ui = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    let target = enroll(&f, meta.clone());
    assert_eq!(f.action(json!({"op":"channel_save","hub":"work","channel_id":"channel","enabled":true,"values":{"credential":"test-only","address":"reply-test"}})).0, 200);
    let hub = f.db("hub.sqlite");
    let channel_frames = || -> Vec<Value> {
        fs::read_to_string(channel.join("frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    let ui_results = || -> Vec<Value> {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|frame| frame["op"] == "composer_result")
            .collect()
    };
    let source = || -> String {
        let sent = f.tool(
            "cito_send",
            json!({"hub":"work","to":"@self","message":"Approve this change?"}),
            meta.clone(),
        );
        let id = sent["signal_id"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        wait(|| {
            !f.rows(&format!("SELECT signal_id FROM local_signals WHERE signal_id='{id}' AND acknowledged_at_ms IS NULL")).is_empty()
            && hub.query("SELECT signal_id FROM hub_channel_deliveries WHERE signal_id=? AND status='delivered'", &[json!(id)]).unwrap().len() == 1
        });
        id
    };
    let assert_unread = |id: &str| {
        assert!(f.rows(&format!(
            "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{id}'"
        ))[0]["acknowledged_at_ms"]
            .is_null());
        assert!(hub
            .query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                &[json!(id)]
            )
            .unwrap()[0]["acked_at_ms"]
            .is_null());
    };
    let untouched = source();
    for op in ["send_signal", "send_reply_from_human", "cito_send"] {
        let id = source();
        let results_before = ui_results().len();
        let result = if op == "cito_send" {
            f.tool("cito_send", json!({"hub":"work","to":target["listener_id"],"message":"Approved","in_reply_to":format!("work:{id}")}), human())
        } else {
            let action = if op == "send_signal" {
                json!({"op":op,"request_id":format!("{op}-{id}"),"to":target["listener_id"],"message":"Approved","in_reply_to":format!("work:{id}")})
            } else {
                json!({"op":op,"request_id":format!("{op}-{id}"),"to_listener_id":target["listener_id"],"text":"Approved","in_reply_to":null,"signal_id":format!("work:{id}")})
            };
            let (status, result) = f.action(action);
            assert_eq!(status, 200, "{result}");
            wait(|| ui_results().len() == results_before + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], true);
            assert_eq!(
                ui_results().last().unwrap()["request_id"],
                format!("{op}-{id}")
            );
            result
        };
        if op == "send_reply_from_human" {
            assert!(!result["signal_id"].as_str().unwrap().contains(':'));
            assert_eq!(
                result["target_display"],
                target["listener_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            );
            assert_eq!(result["error"], Value::Null);
        }
        let reply = result["signal_id"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap();
        wait(|| {
            !f.rows(&format!(
                "SELECT signal_id FROM local_signals WHERE signal_id='{reply}'"
            ))
            .is_empty()
        });
        let delivered = &f.rows(&format!(
            "SELECT message,in_reply_to FROM local_signals WHERE signal_id='{reply}'"
        ))[0];
        assert_eq!(delivered["message"], "Approved");
        assert_eq!(delivered["in_reply_to"], id);
        assert!(f.rows(&format!(
            "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{id}'"
        ))[0]["acknowledged_at_ms"]
            .is_number());
        wait(|| {
            hub.query(
                "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
                &[json!(id)],
            )
            .unwrap()[0]["acked_at_ms"]
                .is_number()
        });
        wait(|| {
            channel_frames()
                .iter()
                .any(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([id]))
        });
        let ack = channel_frames()
            .into_iter()
            .find(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([id]))
            .unwrap();
        assert_eq!(ack["principal_id"], "none#alice");
        let mut external = ack["external_msg_ids"].as_array().unwrap().clone();
        external.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(
            external,
            json!([format!("{id}-1"), format!("{id}-2")])
                .as_array()
                .unwrap()
                .clone()
        );
        assert_unread(&untouched);
    }

    let retried = source();
    hub.execute("CREATE TRIGGER reject_ack BEFORE UPDATE OF acked_at_ms ON hub_signals BEGIN SELECT RAISE(ABORT,'injected ack failure'); END", &[]).unwrap();
    let before = hub
        .query("SELECT COUNT(*) AS n FROM hub_signals", &[])
        .unwrap();
    for op in ["send_signal", "send_reply_from_human", "cito_send"] {
        let args = json!({"to":target["listener_id"],"message":"Retry reply","in_reply_to":format!("work:{retried}"),"idempotency_key":format!("retry-{op}")});
        if op == "cito_send" {
            let result = f.tool_result(op, args, human());
            assert_eq!(result["isError"], true);
            assert!(result.to_string().contains("injected ack failure"));
        } else {
            let mut action = args;
            action["op"] = json!(op);
            let count = ui_results().len();
            let (status, result) = f.action(action);
            assert_eq!(status, 400, "{result}");
            assert!(result.to_string().contains("injected ack failure"));
            wait(|| ui_results().len() == count + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], false);
        }
        assert_unread(&retried);
        assert_eq!(
            hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
                .unwrap(),
            before
        );
        assert!(f
            .rows(&format!(
                "SELECT signal_id FROM pending_hub_acks WHERE signal_id='{retried}'"
            ))
            .is_empty());
    }
    hub.execute("DROP TRIGGER reject_ack", &[]).unwrap();
    let args = json!({"to":target["listener_id"],"message":"Retry reply","in_reply_to":format!("work:{retried}"),"idempotency_key":"retry-send_reply_from_human","op":"send_reply_from_human"});
    let count = ui_results().len();
    let (status, reply) = f.action(args.clone());
    assert_eq!(status, 200, "{reply}");
    wait(|| ui_results().len() == count + 1);
    assert_eq!(ui_results().last().unwrap()["ok"], true);
    assert_eq!(f.action(args).1["signal_id"], reply["signal_id"]);
    wait(|| ui_results().len() == count + 2);
    assert_eq!(ui_results().last().unwrap()["ok"], true);
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap()[0]["n"]
            .as_u64()
            .unwrap(),
        before[0]["n"].as_u64().unwrap() + 1
    );
    wait(|| {
        channel_frames()
            .iter()
            .any(|frame| frame["op"] == "on_acked" && frame["signal_ids"] == json!([retried]))
    });
    assert!(f.rows(&format!(
        "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{retried}'"
    ))[0]["acknowledged_at_ms"]
        .is_number());
    assert!(f
        .rows(&format!(
            "SELECT signal_id FROM pending_hub_acks WHERE signal_id='{retried}'"
        ))
        .is_empty());

    // A local commit failure leaves the reservation pending after the Hub commits.
    // Retrying must reuse the Hub's signal ID and reconcile the local source.
    let recovered = source();
    let local = f.db("state.sqlite");
    local.execute("CREATE TRIGGER reject_sent_commit BEFORE UPDATE ON local_sent_idempotency BEGIN SELECT RAISE(ABORT,'injected local commit failure'); END", &[]).unwrap();
    let args = json!({"hub":"work","to":target["listener_id"],"message":"Recover local commit","in_reply_to":format!("work:{recovered}"),"idempotency_key":"local-commit"});
    let result = f.tool_result("cito_send", args.clone(), human());
    assert_eq!(result["isError"], true);
    assert!(result.to_string().contains("injected local commit failure"));
    let committed = hub.query("SELECT seq,signal_id FROM hub_signals WHERE json_extract(CAST(data AS TEXT),'$.in_reply_to')=?", &[json!(recovered)]).unwrap();
    assert_eq!(committed.len(), 1);
    assert!(hub
        .query(
            "SELECT acked_at_ms FROM hub_signals WHERE signal_id=?",
            &[json!(recovered)]
        )
        .unwrap()[0]["acked_at_ms"]
        .is_number());
    local
        .execute("DROP TRIGGER reject_sent_commit", &[])
        .unwrap();
    let reply = f.tool("cito_send", args, human());
    assert_eq!(
        reply["signal_id"],
        format!("work:{}", committed[0]["signal_id"].as_str().unwrap())
    );
    assert_eq!(reply["seq"], committed[0]["seq"]);
    assert_eq!(hub.query("SELECT seq,signal_id FROM hub_signals WHERE json_extract(CAST(data AS TEXT),'$.in_reply_to')=?", &[json!(recovered)]).unwrap(), committed);
    assert!(f.rows(&format!(
        "SELECT acknowledged_at_ms FROM local_signals WHERE signal_id='{recovered}'"
    ))[0]["acknowledged_at_ms"]
        .is_number());

    let failed = source();
    let before = hub
        .query("SELECT COUNT(*) AS n FROM hub_signals", &[])
        .unwrap();
    for op in ["send_signal", "send_reply_from_human"] {
        for (message, reply_to) in [
            ("   ", format!("work:{failed}")),
            ("Wrong hub", format!("elsewhere:{failed}")),
        ] {
            let count = ui_results().len();
            let request_id = format!("{op}-failure-{count}");
            let (status, result) = f.action(json!({"op":op,"request_id":request_id,"to":target["listener_id"],"message":message,"in_reply_to":reply_to}));
            assert_eq!(status, 400, "{result}");
            wait(|| ui_results().len() == count + 1);
            assert_eq!(ui_results().last().unwrap()["ok"], false);
            assert_eq!(ui_results().last().unwrap()["request_id"], request_id);
            assert_unread(&failed);
        }
    }
    hub.execute("CREATE TRIGGER reject_reply BEFORE INSERT ON hub_signals BEGIN SELECT RAISE(ABORT,'injected reply failure'); END", &[]).unwrap();
    let count = ui_results().len();
    assert_eq!(f.action(json!({"op":"send_signal","to":target["listener_id"],"message":"Rejected publish","in_reply_to":format!("work:{failed}")})).0, 400);
    wait(|| ui_results().len() == count + 1);
    assert_eq!(ui_results().last().unwrap()["ok"], false);
    hub.execute("DROP TRIGGER reject_reply", &[]).unwrap();
    assert_unread(&failed);
    f.tool("cito_unlisten", json!({"hub":"work"}), meta);
    for op in ["send_signal", "send_reply_from_human"] {
        let count = ui_results().len();
        assert_eq!(f.action(json!({"op":op,"to":target["listener_id"],"message":"Revoked target","in_reply_to":format!("work:{failed}")})).0, 400);
        wait(|| ui_results().len() == count + 1);
        assert_eq!(ui_results().last().unwrap()["ok"], false);
        assert_unread(&failed);
    }
    assert_eq!(
        hub.query("SELECT COUNT(*) AS n FROM hub_signals", &[])
            .unwrap(),
        before
    );
    // A request/response barrier drains the channel's stdin before checking absence.
    assert_eq!(
        f.action(json!({"op":"channel_test","hub":"work","channel_id":"channel"}))
            .0,
        200
    );
    assert!(!channel_frames()
        .iter()
        .any(|frame| frame["op"] == "on_acked"
            && frame["signal_ids"].as_array().is_some_and(
                |ids| ids.contains(&json!(failed)) || ids.contains(&json!(untouched))
            )));
}

#[test]
fn ui_ipc_actions_mutate_real_state_and_only_sandbox_host_services() {
    let _lock = serial();
    let owner = Fixture::new(json!({}));
    let manifest = json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],
        "pairing":{"supported":true},"fields":[
            {"key":"credential","scope":"hub","type":"secret","required":true},
            {"key":"address","scope":"user","type":"string","required":true}]});
    let remote_plugin = owner.state.join("hub/plugins/channel");
    fs::create_dir_all(&remote_plugin).unwrap();
    fs::write(remote_plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let (_, hub_port) = owner.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"work":{"url":format!("http://127.0.0.1:{hub_port}")}},"defaultHub":"local"}),
    );
    f.provider("delivered_unconfirmed");
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), manifest.to_string()).unwrap();
    let ui = f.recording_ui();
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    for op in [
        "refresh_available",
        "open_settings",
        "close_settings",
        "open_directory",
        "query_directory",
        "close_directory",
    ] {
        assert_eq!(f.action(json!({"op":op})).0, 200, "{op}");
    }
    for (harness, session_id, error) in [
        ("missing", "s1", "UNSUPPORTED_HARNESS: Unknown harness"),
        ("channel", "s1", "UNSUPPORTED_HARNESS: Unknown harness"),
        (
            "fixture",
            "closed-session",
            "BINDING_REQUIRED: Caller session mismatch",
        ),
    ] {
        let (status, result) = f.action(json!({"op":"listen_session","hub":"local","harness":harness,"session_id":session_id,"about":"bad"}));
        assert_eq!(status, 400, "{result}");
        assert_eq!(result["error"], error);
        assert!(f
            .rows("SELECT * FROM local_listeners WHERE harness <> 'human'")
            .is_empty());
        assert!(f
            .db("hub.sqlite")
            .query(
                "SELECT * FROM hub_kv WHERE json_extract(CAST(value AS TEXT),'$.harness') <> 'human'",
                &[],
            )
            .unwrap()
            .is_empty());
    }
    assert_eq!(f.action(json!({"op":"listen_session","harness":"fixture","session_id":"session-1","about":"IPC receiver","scope":"workspace"})).0,200);
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    let id = row["listener_id"].clone();
    assert_eq!(
        f.action(json!({"op":"switch_hub","from_hub":"local","to_hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='local' AND harness='fixture'")[0]
            ["status"],
        "UNLISTENED"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["status"],
        "ACTIVE"
    );
    let hub_db = owner.db("hub/hub.sqlite");
    assert_eq!(
        f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
            ["scope"],
        "workspace"
    );
    assert_eq!(
        hub_db
            .query(
                "SELECT scope FROM hub_kv WHERE key=?",
                std::slice::from_ref(&id)
            )
            .unwrap()[0]["scope"],
        "workspace"
    );
    for scope in ["self", "workspace"] {
        assert_eq!(
            f.action(json!({"op":"set_scope","hub":"work","listener_id":id,"scope":scope}))
                .0,
            200
        );
        assert_eq!(
            f.rows("SELECT scope FROM local_listeners WHERE hub='work' AND harness='fixture'")[0]
                ["scope"],
            scope
        );
        let registration = hub_db
            .query(
                "SELECT scope,CAST(value AS TEXT) AS value FROM hub_kv WHERE key=?",
                std::slice::from_ref(&id),
            )
            .unwrap();
        assert_eq!(registration[0]["scope"], scope);
        let value: Value =
            serde_json::from_str(registration[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value["scope"], scope);
    }
    for op in ["setup_apply", "setup_remove_app"] {
        assert_eq!(f.action(json!({"op":op,"harness":"fixture"})).0, 200);
    }
    let calls = fs::read_to_string(f.state.join("plugins/fixture/calls.jsonl")).unwrap();
    assert!(calls
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "setup_remove"));
    for op in ["channel_save", "channel_test", "channel_pair"] {
        let (s, v) = f.action(json!({"op":op,"channel_id":"missing"}));
        assert_eq!(s, 400);
        assert_eq!(
            v["error"],
            "INVALID_INPUT: Unknown human channel plugin: missing"
        );
    }
    let (status, saved) = f.action(
        json!({"op":"channel_save","hub":"work","channel_id":"channel",
        "enabled":true,"values":{"credential":"fixture-secret","address":"fixture-account"}}),
    );
    assert_eq!(status, 200, "{saved}");
    let channel = saved["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "channel")
        .unwrap();
    assert_eq!(channel["configured"], true);
    let service = hub_db
        .query(
            "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
            &[],
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(service[0]["config_json"].as_str().unwrap()).unwrap(),
        json!({"credential":"fixture-secret"})
    );
    let binding = hub_db.query("SELECT enabled,user_config_json FROM hub_human_channels WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(binding[0]["enabled"], 1);
    assert_eq!(
        serde_json::from_str::<Value>(binding[0]["user_config_json"].as_str().unwrap()).unwrap(),
        json!({"address":"fixture-account"})
    );
    let (status, tested) =
        f.action(json!({"op":"channel_test","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{tested}");
    assert_eq!(tested["ok"], true);
    assert_eq!(tested["detail"], "Fixture test succeeded");
    let (status, paired) =
        f.action(json!({"op":"channel_pair","hub":"work","channel_id":"channel"}));
    assert_eq!(status, 200, "{paired}");
    let code = paired["code"].as_str().unwrap();
    assert!(code.starts_with("cito-"));
    assert_eq!(
        paired["instruction"],
        format!("Send {code} to fixture-account")
    );
    let pairing = hub_db.query("SELECT code,instruction,expires_ms,consumed_ms FROM hub_pairing_codes WHERE plugin_id='channel' AND principal_id='none#alice'", &[]).unwrap();
    assert_eq!(pairing[0]["code"], paired["code"]);
    assert_eq!(pairing[0]["instruction"], paired["instruction"]);
    assert_eq!(pairing[0]["expires_ms"], paired["expires_at_ms"]);
    assert!(pairing[0]["consumed_ms"].is_null());
    wait(|| {
        let frames: Vec<Value> = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        frames.iter().any(|v| {
            v["op"] == "channel_test_result"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["ok"] == true
                && v["detail"] == "Fixture test succeeded"
        }) && frames.iter().any(|v| {
            v["op"] == "pair_code"
                && v["hub"] == "work"
                && v["channel_id"] == "channel"
                && v["code"] == paired["code"]
                && v["instruction"] == paired["instruction"]
                && v["expires_at_ms"] == paired["expires_at_ms"]
        })
    });
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":true}))
            .0,
        200
    );
    assert!(f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    assert_eq!(
        f.action(json!({"op":"set_launch_at_login","enabled":false}))
            .0,
        200
    );
    assert!(!f
        .home
        .join("Library/LaunchAgents/com.tefx.cito.daemon.plist")
        .exists());
    assert_eq!(
        f.action(json!({"op":"set_desktop_banner","enabled":false}))
            .0,
        200
    );
    let p = port();
    assert_eq!(f.action(json!({"op":"set_host_hub","enabled":true,"port":p,"bind":"127.0.0.1","auth_mode":"none"})).0,200);
    assert!(rpc(p, "kv_snapshot", json!({}))["entries"].is_array());
    let configured: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    let token = configured["host_hub"]["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(
        f.action(json!({"op":"set_host_hub","enabled":false})).0,
        200
    );
    // Disconnect now revokes this harness's listeners. Reconnect explicitly before testing delivery.
    assert!(f
        .rows("SELECT * FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"setup_apply","harness":"fixture"})).0,
        200
    );
    assert_eq!(f.action(json!({"op":"listen_session","hub":"work","harness":"fixture","session_id":"session-1","about":"IPC receiver","scope":"workspace"})).0,200);
    let sent = f.action(json!({"op":"send_signal","hub":"work","to":id,"message":"Agent inbox"}));
    assert_eq!(sent.0, 200, "{}", sent.1);
    wait(|| !f.rows("SELECT * FROM local_signals").is_empty());
    f.tool(
        "cito_inbox",
        json!({"hub":"work"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    enroll(&f, human());
    f.tool(
        "cito_send",
        json!({"hub":"local","to":"@self","message":"Human inbox"}),
        json!({"expected_harness":"fixture","expected_session_id":"session-1"}),
    );
    wait(|| {
        !f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
            .is_empty()
    });
    assert_eq!(
        f.action(json!({"op":"ack_signals","hub":"local","ack_all":true}))
            .0,
        200
    );
    assert!(f
        .rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NULL")
        .is_empty());
    assert_eq!(
        f.action(json!({"op":"unlisten","hub":"work","listener_id":id}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"unlisten_all"})).0, 200);
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"extra","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_toggle_hub","hub":"extra","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_remove_hub","hub":"extra"})).0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"hub_action","hub":"work","action":"reconnect"}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"quit"})).0, 200);
    wait(|| f.exited(daemon));
    let g = Fixture::new(json!({}));
    fs::write(g.state.join("retain-data"), "keep").unwrap();
    let daemon = g.daemon(&[]);
    assert_eq!(
        g.action(json!({"op":"setup_uninstall_all","purge_data":false}))
            .0,
        200
    );
    wait(|| g.exited(daemon));
    assert_eq!(
        fs::read_to_string(g.state.join("retain-data")).unwrap(),
        "keep"
    );
}

#[test]
fn snapshot_unread_count_precedes_activity_limit_and_utf8_frame_bounding() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let daemon = f.daemon(&[]);
    enroll(&f, human());
    let row = f.rows("SELECT * FROM local_listeners WHERE harness='human'")[0].clone();
    f.stop(daemon);
    let db = f.db("state.sqlite");
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000;
    db.transaction(|c|{for i in 0..100 {c.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state) VALUES('local',?,?,?,?,'bob','laptop','Sender',?,'2030-01-01T00:00:00.000Z',?,'STORED_PASSIVE')",&[json!(format!("sig-{i}")),row["listener_id"].clone(),json!(i),row["binding_generation"].clone(),json!("🦀".repeat(10000)),json!(expires)])?;}Ok(())}).unwrap();
    f.daemon(&[]);
    let (s, frame) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(s, 200);
    assert_eq!(frame["unread_count"], 100);
    assert_eq!(frame["activity"].as_array().unwrap().len(), 80);
    assert!(serde_json::to_vec(&frame).unwrap().len() + 1 < 256 * 1024);
    assert!(frame["activity"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["message"].as_str().unwrap().chars().count() <= 512));
    assert_eq!(
        db.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        4
    );
    assert_eq!(
        db.query("SELECT COUNT(*) AS n FROM local_signals", &[])
            .unwrap()[0]["n"],
        100
    );
}
fn human() -> Value {
    serde_json::from_str(HUMAN).unwrap()
}
fn enroll(f: &Fixture, meta: Value) -> Value {
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver","scope":"workspace"}),
        meta,
    )
}
fn rpc(p: u16, op: &str, params: Value) -> Value {
    let mut v = params;
    v["op"] = json!(op);
    let (s, r) = http(p, "/v1/rpc", Some(&v));
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["ok"], true, "{r}");
    r["result"].clone()
}
#[test]
fn mcp_http_initialize_tool_schemas_and_malformed_boundaries() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let init=f.mcp("initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}));
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    let list = f.mcp("tools/list", json!({}));
    let names: Vec<_> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "cito_listen",
            "cito_unlisten",
            "cito_find",
            "cito_send",
            "cito_inbox"
        ]
    );
    let find = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "cito_find")
        .unwrap();
    assert_eq!(
        find["inputSchema"]["properties"]["harness"]["pattern"],
        "^[a-z0-9][a-z0-9_-]{0,31}$"
    );
    assert_eq!(
        f.tool("cito_find", json!({"harness":"1agent"}), human())["listeners"],
        json!([])
    );
    for v in [
        json!([]),
        json!(42),
        json!(true),
        json!("request"),
        Value::Null,
    ] {
        for path in ["/ui", "/control", "/action", "/mcp"] {
            assert_eq!(http(f.port, path, Some(&v)).0, 400);
        }
        assert_eq!(f.mcp("tools/call", v.clone())["isError"], true);
        assert_ne!(f.tool_result("cito_find", json!({}), v)["isError"], true);
    }
    assert_eq!(f.mcp("ping", json!({})), json!({}));
}
#[test]
fn v29_readable_aliases_reject_ambiguous_live_prefixes_even_for_one_principal() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    let canonical = target["canonical_listener_id"].as_str().unwrap();
    let prefix = &canonical[2..6];
    for length in [8, 12, 16] {
        let query = format!("alice/fixture#{}", &canonical[2..2 + length]);
        let found = f.tool("cito_find", json!({"query":query}), human());
        assert_eq!(found["matched_count"], 1, "{found}");
        assert_eq!(found["listeners"][0]["listener_id"], canonical);
    }
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
        format!("local:alice/fixture#{prefix}"),
    ] {
        assert_eq!(
            f.tool(
                "cito_send",
                json!({"to":to,"message":"Alias routing"}),
                human()
            )["to"],
            target["listener_id"]
        );
    }
    let duplicate = format!("c_{prefix}ffffffffffff");
    let row = json!({"listener_id":duplicate,"public_id":format!("alice-{}",&duplicate[2..]),"owner_user":"alice","principal_id":"none#alice","harness":"fixture","state":"ACTIVE","scope":"workspace","updated_at_ms":cito_native::core::plugins::now_ms(),"binding_generation":1});
    f.db("hub.sqlite").execute("INSERT INTO hub_kv(key,revision,created_ms,value,principal_id,scope) VALUES(?,1,?,CAST(? AS BLOB),'none#alice','workspace')",&[json!(duplicate),json!(cito_native::core::plugins::now_ms()),json!(row.to_string())]).unwrap();
    wait(|| {
        f.tool("cito_find", json!({"harness":"fixture"}), human())["listeners"]
            .as_array()
            .unwrap()
            .len()
            == 2
    });
    for to in [
        format!("alice/fixture#{prefix}"),
        format!("fixture#{prefix}"),
        format!("alice#{prefix}"),
    ] {
        let result = f.tool_result(
            "cito_send",
            json!({"to":to,"message":"Must disambiguate"}),
            human(),
        );
        assert_eq!(result["isError"], true, "{result}");
        let error: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(error["error"]["code"], "INVALID_INPUT");
        assert_eq!(
            error["error"]["message"],
            "Ambiguous target; use full listener_id"
        );
    }
}

#[test]
fn v29_malformed_inspection_stays_unknown_and_surfaces_error() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.provider("delivered");
    fs::write(f.state.join("plugins/fixture/options.json"), "{").unwrap();
    let recorder = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", recorder.to_str().unwrap()),
    ]);
    f.action(json!({"op":"open_settings"}));
    let mut settings = Value::Null;
    wait(|| {
        settings = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .rfind(|v| v["op"] == "settings")
            .unwrap_or(Value::Null);
        settings.is_object()
    });
    let app = settings["settings"]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "fixture")
        .unwrap();
    assert_eq!(app["status"], "error", "{settings}");
    assert!(app["configured"].is_null());
    assert!(!app["detail"].as_str().unwrap().is_empty());
}

#[test]
fn v29_sender_auto_enrollment_readable_reply_and_clear_close_the_pathway() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let sent = f.tool(
        "cito_send",
        json!({"to":"@self","message":"Agent initiated"}),
        meta.clone(),
    );
    assert_eq!(sent["ok"], true, "{sent}");
    let sender = f.rows("SELECT * FROM local_listeners WHERE harness='fixture'")[0].clone();
    assert_eq!(sender["status"], "ACTIVE");
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE message='Agent initiated'")
            .len()
            == 1
    });
    let early = f.action(json!({"op":"get_snapshot"})).1;
    assert!(
        early["activity"][0]["from_display_address"]
            .as_str()
            .unwrap()
            .contains("/fixture#"),
        "{early}"
    );
    let (status, snapshot) = f.action(json!({"op":"query_directory","filter":"all"}));
    assert_eq!(status, 200, "{snapshot}");
    let activity = &snapshot["activity"][0];
    assert_eq!(activity["from_scoped_id"], sender["public_id"]);
    assert!(activity["from_display_address"]
        .as_str()
        .unwrap()
        .contains("/fixture#"));
    let reply = f.tool("cito_send",json!({"to":activity["from_display_address"],"message":"Human reply","in_reply_to":activity["signal_id"]}),human());
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["to"], sender["public_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    assert_eq!(f.action(json!({"op":"clear_activity_history"})).0, 200);
    assert!(f.rows("SELECT * FROM local_signals").is_empty());
    f.tool("cito_unlisten", json!({}), meta.clone());
    assert_eq!(
        f.tool(
            "cito_send",
            json!({"to":"@self","message":"Explicitly paused"}),
            meta
        )["ok"],
        true
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}

#[test]
fn local_mcp_listen_find_send_inbox_ack_idempotency_and_channel_detach() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    let target = enroll(&f, meta.clone());
    assert_eq!(target["hub"], "local");
    assert_eq!(target["scope"], "workspace");
    let find = f.tool("cito_find", json!({"query":"Native receiver"}), human());
    assert!(find["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["scoped_id"] == target["listener_id"]
            && v["listener_id"] == target["canonical_listener_id"]));
    let long = "Context payload segment ".repeat(100).trim().to_string();
    let args = json!({"to":target["listener_id"],"message":long,"idempotency_key":"stable"});
    let sent = f.tool("cito_send", args.clone(), human());
    assert_eq!(sent["status"], "wake_accepted");
    let again = f.tool("cito_send", args, human());
    assert_eq!(sent["signal_id"], again["signal_id"]);
    wait(|| f.rows("SELECT * FROM local_signals").len() == 1);
    assert!(
        f.rows("SELECT acknowledged_at_ms FROM local_signals")[0]["acknowledged_at_ms"].is_null()
    );
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"][0]["message"], long);
    assert_eq!(inbox["acknowledged_count"], 1);
    assert_eq!(
        f.tool("cito_inbox", json!({}), meta.clone())["signals"],
        json!([])
    );
    assert_eq!(
        f.tool("cito_unlisten", json!({}), meta)["status"],
        "unlistened"
    );
    assert_eq!(
        f.rows("SELECT status FROM local_listeners WHERE harness='fixture'")[0]["status"],
        "UNLISTENED"
    );
}
#[test]
fn confirmed_delivery_business_ack_and_pending_hub_ack_reconciliation() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    f.daemon(&[]);
    let target = enroll(&f, meta);
    f.tool(
        "cito_send",
        json!({"to":target["listener_id"],"message":"confirmed"}),
        human(),
    );
    wait(|| {
        f.rows("SELECT * FROM local_signals WHERE acknowledged_at_ms IS NOT NULL")
            .len()
            == 1
    });
    wait(|| {
        f.db("hub.sqlite")
            .query(
                "SELECT * FROM hub_signals WHERE acked_at_ms IS NOT NULL",
                &[],
            )
            .unwrap()
            .len()
            == 1
    });
}
#[test]
fn standalone_http_hub_dedup_atomic_batch_claims_and_business_ack() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let (_, p) = f.hub();
    assert_eq!(
        http(p, "/health", None).1["version"],
        env!("CARGO_PKG_VERSION")
    );
    let id = "c_1111111111111111";
    rpc(
        p,
        "kv_put",
        json!({"key":id,"data_b64":B64.encode(json!({"listener_id":id,"principal_id":"none#alice","owner_user":"alice","harness":"fixture","session_id":"s","scope":"workspace","state":"ACTIVE","binding_generation":1,"channels":{"a":{"state":"ACTIVE"},"b":{"state":"ACTIVE"}}}).to_string())}),
    );
    for signal in ["one", "two", "three"] {
        let publish = json!({"subject":format!("cito.signal.{id}"),"data_b64":B64.encode(json!({"signal_id":signal,"to":id,"target_generation":1,"message":"hello"}).to_string()),"opts":{"msgID":signal}});
        assert_eq!(
            rpc(p, "js_publish", publish.clone())["seq"],
            rpc(p, "js_publish", publish)["seq"]
        );
    }
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"b","signal_ids":["two"]})
        )["claimed"],
        true
    );
    let conflict = rpc(
        p,
        "signal_claim_dispatch",
        json!({"node_id":id,"channel_id":"a","signal_ids":["one","two"]}),
    );
    assert_eq!(conflict["claimed"], false);
    assert_eq!(conflict["claimed_ids"], json!([]));
    let db = f.db("hub/hub.sqlite");
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='one'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
    rpc(
        p,
        "signal_ack",
        json!({"node_id":id,"channel_id":"b","signal_ids":["two"]}),
    );
    assert_eq!(
        rpc(
            p,
            "signal_claim_dispatch",
            json!({"node_id":id,"channel_id":"a","signal_ids":["three","two"]})
        )["acked_ids"],
        json!(["two"])
    );
    assert!(db
        .query(
            "SELECT claimed_by_channel FROM hub_signals WHERE signal_id='three'",
            &[]
        )
        .unwrap()[0]["claimed_by_channel"]
        .is_null());
}
#[test]
fn two_remote_hubs_isolate_identifiers_and_offline_inbox_remains_readable() {
    let _lock = serial();
    let a = Fixture::new(json!({}));
    let b = Fixture::new(json!({}));
    let (ha, pa) = a.hub();
    let (hb, pb) = b.hub();
    let f = Fixture::new(
        json!({"hubs":{"personal":{"url":format!("http://127.0.0.1:{pa}"),"user":"alice"},"work":{"url":format!("http://127.0.0.1:{pb}"),"user":"alice"}},"defaultHub":"personal"}),
    );
    let meta = f.provider("delivered_unconfirmed");
    f.daemon(&[]);
    for hub in ["personal", "work"] {
        let target = f.tool(
            "cito_listen",
            json!({"hub":hub,"about":"Multi hub","scope":"workspace"}),
            meta.clone(),
        );
        let sent = f.tool(
            "cito_send",
            json!({"to":target["listener_id"],"message":hub}),
            human(),
        );
        assert!(sent["signal_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{hub}:")));
    }
    wait(|| f.rows("SELECT * FROM local_signals").len() == 2);
    a.stop(ha);
    b.stop(hb);
    let inbox = f.tool("cito_inbox", json!({}), meta.clone());
    assert_eq!(inbox["signals"].as_array().unwrap().len(), 2);
    assert_eq!(f.rows("SELECT * FROM pending_hub_acks").len(), 2);
    f.tool("cito_unlisten", json!({}), meta);
    assert!(f
        .rows("SELECT status FROM local_listeners WHERE harness='fixture'")
        .iter()
        .all(|v| v["status"] == "UNLISTENED"));
}
#[test]
fn live_user_reload_and_auth_mode_canonicalization_survive_restart() {
    let _lock = serial();
    let mut f =
        Fixture::new(json!({"user":"alice","hubs":{"local":{"url":"local://","authMode":"NONE"}}}));
    f.env.remove("CITO_USER");
    let daemon = f.daemon(&[]);
    let a = enroll(&f, human());
    assert!(a["listener_id"].as_str().unwrap().starts_with("alice-"));
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    config["user"] = json!("bob");
    fs::write(&f.config, config.to_string()).unwrap();
    let b = enroll(&f, human());
    assert!(
        b["listener_id"].as_str().unwrap().starts_with("bob-"),
        "{b}"
    );
    f.stop(daemon);
    f.daemon(&[]);
    assert!(enroll(&f, human())["listener_id"]
        .as_str()
        .unwrap()
        .starts_with("bob-"));
}
#[test]
fn r1_remote_schema_hub_only_saves_never_create_or_resume_personal_rows() {
    let _lock = serial();
    let owner = Fixture::new(json!({}));
    let manifest = json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Messenger",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"pairing":{"supported":true},"fields":[
        {"key":"credential","scope":"hub","type":"secret","required":true},
        {"key":"address","scope":"user","type":"string","required":true}]});
    let directory = owner.state.join("hub/plugins/channel");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
    let (_, p) = owner.hub();
    let f = Fixture::new(
        json!({"hubs":{"local":{"url":"local://"},"remote":{"url":format!("http://127.0.0.1:{p}")}},"defaultHub":"local"}),
    );
    let mut conflicting = manifest;
    conflicting["fields"][0]["scope"] = json!("user");
    conflicting["fields"][1]["scope"] = json!("hub");
    let directory = f.state.join("plugins/channel");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("plugin.json"), conflicting.to_string()).unwrap();
    f.daemon(&[]);
    let rows = || {
        owner
            .db("hub/hub.sqlite")
            .query(
                "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
                &[],
            )
            .unwrap()
    };
    for op in [
        "save_hub_service",
        "channel_save",
        "channel_pair",
        "channel_test",
    ] {
        let (status,v)=f.action(json!({"op":op,"scope":"hub","hub":"remote","channel_id":"channel","enabled":true,"user_config":{"address":"injected"},"display_label":"injected","values":{"credential":op,"address":"must-not-save"}}));
        assert_eq!(status, 200, "{v}");
        assert!(rows().is_empty());
        let cfg = owner
            .db("hub/hub.sqlite")
            .query(
                "SELECT config_json FROM hub_plugin_services WHERE plugin_id='channel'",
                &[],
            )
            .unwrap();
        let cfg: Value = serde_json::from_str(cfg[0]["config_json"].as_str().unwrap()).unwrap();
        assert_eq!(
            cfg,
            json!({"credential":op}),
            "Selected remote schema must override conflicting local field scopes"
        );
    }
    assert_eq!(f.action(json!({"op":"channel_save","hub":"remote","channel_id":"channel","enabled":true,"values":{"address":"personal"}})).0,200);
    assert_eq!(rows()[0]["enabled"], 1);
    assert_eq!(
        f.action(
            json!({"op":"channel_toggle","hub":"remote","channel_id":"channel","enabled":false})
        )
        .0,
        200
    );
    let paused = rows();
    assert_eq!(f.action(json!({"op":"save_hub_service","hub":"remote","channel_id":"channel","values":{"credential":"rotated"}})).0,200);
    assert_eq!(rows(), paused);
    assert_eq!(f.action(json!({"op":"channel_test","hub":"remote","channel_id":"channel","values":{"address":"candidate"}})).0,200);
    assert_eq!(rows(), paused, "Non-persisting tests cannot write bindings");
}

#[test]
fn ui_invites_and_config_mutations_validate_before_persistent_effects() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    for invite in ["[]", "42", "true", "\"invite\"", "null"] {
        let (s, v) = f.action(json!({"op":"paste_hub_invite","invite":invite}));
        assert_eq!(s, 400);
        assert_eq!(v["error"], "INVALID_INPUT: Invalid hub invite");
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    assert_eq!(f.action(json!({"op":"paste_hub_invite","invite":"{\"name\":\"work\",\"url\":\"local://\",\"authMode\":\"none\"}"})).0,200);
    assert_eq!(
        f.action(json!({"op":"set_desktop_banner","enabled":false}))
            .0,
        200
    );
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","name":"other","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    let config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    assert_eq!(config["desktopBannerEnabled"], false);
    assert_eq!(config["hubs"]["other"]["url"], "local://");
    for (name,invite) in [
        ("127-0-0-1",format!("http://127.0.0.1:{}",port())),
        ("joined",format!("cito setup --hub joined --url http://127.0.0.1:{} --auth token --token fixture-token --default",port())),
        ("json",json!({"name":"json","url":format!("http://127.0.0.1:{}",port()),"auth_mode":"none"}).to_string()),
        ("equals",format!("cito setup --hub=equals --url=http://127.0.0.1:{} --default",port())),
    ] {
        let (status,value)=f.action(json!({"op":"paste_hub_invite","invite":invite})); assert_eq!(status,200,"{value}");
        let config:Value=serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
        assert!(config["hubs"][name].is_object()); assert_eq!(config["default_hub"],name);
    }
    let (status, error) = f.action(json!({"op":"test_hub","hub":"json"}));
    assert_eq!(status, 400);
    let state = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        state["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "json")
            .unwrap()["error"],
        error["error"]
    );
    assert_eq!(
        f.action(json!({"op":"setup_save_hub","edit":true,"original_name":"json","name":"json","url":"local://","auth_mode":"none"}))
            .0,
        200
    );
    assert_eq!(f.action(json!({"op":"test_hub","hub":"json"})).0, 200);
    let state = f.action(json!({"op":"get_snapshot"})).1;
    assert!(state["settings"]["hubs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "json")
        .unwrap()["error"]
        .is_null());
}
#[test]
fn r1_background_hub_recovery_clears_transient_errors_but_preserves_validation_errors() {
    let _lock = serial();
    let host = Fixture::new(json!({}));
    let (server, port) = host.hub();
    let f = Fixture::new(
        json!({"hubs":{"shared":{"url":format!("http://127.0.0.1:{port}")}},"defaultHub":"shared"}),
    );
    let recorder = f.recording_ui();
    f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", recorder.to_str().unwrap()),
    ]);
    host.stop(server);
    let (status, error) = f.action(json!({"op":"test_hub","hub":"shared"}));
    assert_eq!(status, 400);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .starts_with("NATS_UNAVAILABLE:"));
    let failed = f.action(json!({"op":"get_snapshot"})).1;
    for hubs in [&failed["hubs"], &failed["settings"]["hubs"]] {
        assert_eq!(
            hubs.as_array()
                .unwrap()
                .iter()
                .find(|h| h["name"] == "shared")
                .unwrap()["error"],
            error["error"]
        );
    }
    let frames_path = f.home.join("ui-frames");
    wait(|| {
        fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .contains(error["error"].as_str().unwrap())
    });
    let before = fs::read_to_string(&frames_path).unwrap().len();
    host.spawn(
        NATIVE,
        &[
            "hub",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data-dir",
            host.state.join("hub").to_str().unwrap(),
            "--auth",
            "none",
        ],
        &[],
    );
    // No UI or MCP request drives recovery: the daemon's background transport
    // work must publish a fresh state before we read either projection.
    wait(|| {
        fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .get(before..)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|frame| {
                frame["op"] == "state"
                    && frame["hubs"].as_array().is_some_and(|hubs| {
                        hubs.iter().any(|h| {
                            h["name"] == "shared"
                                && h["status"] == "connected"
                                && h["error"].is_null()
                        })
                    })
            })
    });
    let recovered = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        f.action(json!({"op":"open_settings","hub":"shared"})).0,
        200
    );
    let mut settings = Value::Null;
    wait(|| {
        settings = fs::read_to_string(&frames_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .rfind(|frame| frame["op"] == "settings" && frame["hub"] == "shared")
            .map(|frame| frame["settings"].clone())
            .unwrap_or(Value::Null);
        settings.is_object()
    });
    for hubs in [
        &recovered["hubs"],
        &recovered["settings"]["hubs"],
        &settings["hubs"],
    ] {
        assert!(hubs
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"]
            .is_null());
    }
    let (status, invalid) =
        f.action(json!({"op":"setup_save_hub","name":"shared","url":"invalid"}));
    assert_eq!(status, 400);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .starts_with("INVALID_INPUT:"));
    // A healthy connection must not hide a rejected configuration edit, even
    // after another background poll has observed the recovered transport.
    assert_eq!(f.action(json!({"op":"refresh_available"})).0, 200);
    let snapshot = f.action(json!({"op":"get_snapshot"})).1;
    assert_eq!(
        snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "shared")
            .unwrap()["error"],
        invalid["error"]
    );
}

#[test]
fn removing_or_disabling_default_hubs_persists_a_restartable_fallback() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for (key, op, remaining, expected) in [
        (
            "default_hub",
            "setup_remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "remove_hub",
            json!({"local":{"url":"local://"}}),
            "local",
        ),
        ("default_hub", "remove_hub", json!({}), "local"),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"personal":{"url":"local://","enabled":false},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "remove_hub",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
        (
            "defaultHub",
            "setup_remove_hub",
            json!({"local":{"url":"local://","disabled":true},"shared":{"url":"local://"}}),
            "shared",
        ),
        (
            "default_hub",
            "setup_toggle_hub",
            json!({"local":{"url":"local://"},"shared":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            "hub_action",
            json!({"local":{"url":"local://","enabled":false},"shared":{"url":"local://","enabled":true},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut initial = json!({"hubs":remaining});
        initial["hubs"]["work"] = json!({"url":"local://"});
        initial[key] = json!("work");
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
        let (status, response) =
            f.action(json!({"op":op,"hub":"work","enabled":false,"action":"disconnect"}));
        assert_eq!(status, 200, "{op}: {response}");
        let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(saved["default_hub"], expected, "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        if matches!(op, "setup_toggle_hub" | "hub_action") {
            assert_eq!(saved["hubs"]["work"]["enabled"], false);
        } else {
            assert!(saved["hubs"].get("work").is_none());
        }
        assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], expected);
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["defaultHub"], expected);
        assert!(inspection["hubs"].get(expected).is_some());
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
        f.stop(daemon);
        let restarted = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        f.stop(restarted);
    }
}
#[test]
fn dual_default_hub_aliases_allow_cli_removal_and_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({
        "defaultHub":"work",
        "default_hub":"work",
        "hubs":{"local":{"url":"local://"},"work":{"url":"local://"}}
    }));
    let (ok, out, err) = f.command(
        NATIVE,
        &["setup", "--remove-hub", "work", "--no-login-item", "--json"],
        "",
    );
    assert!(ok, "{out}\n{err}");
    let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    assert_eq!(saved["default_hub"], "local");
    assert!(saved.get("defaultHub").is_none());
    assert!(saved["hubs"].get("work").is_none());
    let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let inspection: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(inspection["defaultHub"], "local");
    assert_eq!(inspection["hubs"]["local"]["url"], "local://");
    let daemon = f.daemon(&[]);
    let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
    assert_eq!(status, 200, "{snapshot}");
    assert_eq!(snapshot["hub"], "local");
    let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
    assert!(ok, "{out}\n{err}");
    let found: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(found["ok"], true);
    assert_eq!(found["listeners"], json!([]));
    f.stop(daemon);
}
#[test]
fn reloading_disabled_defaults_selects_enabled_hubs_unless_overridden() {
    let _lock = serial();
    let f = Fixture::new(json!({"default_hub":"work","hubs":{"work":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
    for (key, hubs, expected) in [
        (
            "default_hub",
            json!({"work":{"url":"local://","enabled":false},"local":{"url":"local://"},"alpha":{"url":"local://"}}),
            "local",
        ),
        (
            "defaultHub",
            json!({"work":{"url":"local://","disabled":true},"local":{"url":"local://","enabled":false},"shared":{"url":"local://"},"zeta":{"url":"local://"}}),
            "shared",
        ),
    ] {
        let mut config = json!({"hubs":hubs});
        config[key] = json!("work");
        fs::write(&f.config, config.to_string()).unwrap();
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["hub"], expected);
        let found = f.tool("cito_find", json!({"user":"absent"}), human());
        assert_eq!(found["ok"], true, "{found}");
        assert_eq!(found["listeners"], json!([]));
        assert_eq!(fs::read_to_string(&f.config).unwrap(), config.to_string());
    }
    f.stop(daemon);
    let overridden = f.daemon(&[("CITO_HUB", "work")]);
    assert_eq!(f.action(json!({"op":"get_snapshot"})).1["hub"], "work");
    f.stop(overridden);
}
#[test]
fn disabled_hub_alias_reenable_paths_activate_slots_across_restart() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    for op in ["setup_toggle_hub", "hub_action", "cli"] {
        let initial = json!({"defaultHub":"local","hubs":{
            "local":{"url":"local://","disabled":true},
            "work":{"url":"local://","enabled":true,"disabled":true}
        }});
        fs::write(&f.config, initial.to_string()).unwrap();
        let daemon = f.daemon(&[]);
        let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
        assert_eq!(status, 200, "{snapshot}");
        let work = snapshot["settings"]["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hub| hub["name"] == "work")
            .unwrap();
        assert_eq!(work["enabled"], false, "{op}: {work}");
        let unavailable =
            f.tool_result("cito_find", json!({"hub":"work","user":"absent"}), human());
        assert_eq!(unavailable["isError"], true, "{unavailable}");
        if op == "cli" {
            let (ok, out, err) = f.command(
                NATIVE,
                &["setup", "--enable-hub", "work", "--json", "--no-login-item"],
                "",
            );
            assert!(ok, "{out}\n{err}");
        } else {
            let (status, response) =
                f.action(json!({"op":op,"hub":"work","enabled":true,"action":"reconnect"}));
            assert_eq!(status, 200, "{op}: {response}");
        }
        let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(saved["hubs"]["work"]["enabled"], true, "{op}: {saved}");
        assert!(
            saved["hubs"]["work"].get("disabled").is_none(),
            "{op}: {saved}"
        );
        assert_eq!(saved["default_hub"], "work", "{op}: {saved}");
        assert!(saved.get("defaultHub").is_none());
        for restart in [false, true] {
            let running = if restart { f.daemon(&[]) } else { daemon };
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["hub"], "work", "{op}: {snapshot}");
            let work = snapshot["settings"]["hubs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "work")
                .unwrap();
            assert_eq!(work["enabled"], true, "{op}: {work}");
            let found = f.tool("cito_find", json!({"hub":"work","user":"absent"}), human());
            assert_eq!(found["ok"], true, "{op}: {found}");
            assert_eq!(found["listeners"], json!([]));
            f.stop(running);
        }
    }
}
#[test]
fn disabled_hub_alias_saves_preserve_effective_enablement() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    f.daemon(&[]);
    for op in ["setup_save_hub", "save_hub"] {
        for (fields, enabled) in [
            (json!({"disabled":true}), false),
            (json!({"enabled":true,"disabled":true}), false),
            (json!({"enabled":false,"disabled":false}), false),
            (json!({"enabled":true,"disabled":false}), true),
        ] {
            let mut cfg = fields;
            cfg["url"] = json!("local://");
            let action = if op == "setup_save_hub" {
                cfg["op"] = json!(op);
                cfg["name"] = json!("work");
                cfg
            } else {
                json!({"op":op,"name":"work","config":cfg})
            };
            let (status, response) = f.action(action.clone());
            assert_eq!(status, 200, "{action}: {response}");
            let saved: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
            assert_eq!(
                saved["hubs"]["work"]["enabled"], enabled,
                "{action}: {saved}"
            );
            assert!(
                saved["hubs"]["work"].get("disabled").is_none(),
                "{action}: {saved}"
            );
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            let work = snapshot["settings"]["hubs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hub| hub["name"] == "work")
                .unwrap();
            assert_eq!(work["enabled"], enabled, "{action}: {work}");
            let result = f.tool_result("cito_find", json!({"hub":"work","user":"absent"}), human());
            assert_eq!(result["isError"] == true, !enabled, "{action}: {result}");
        }
    }
}
#[test]
fn hub_saves_reject_invalid_candidates_without_poisoning_daemon() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://"}}}));
    let daemon = f.daemon(&[]);
    let before = fs::read(&f.config).unwrap();
    let invalid = [
        json!({"user":"alice smith"}),
        json!({"user":""}),
        json!({"user":"a".repeat(65)}),
        json!({"user":"alice@work"}),
        json!({"user":42}),
        json!({"user":false}),
        json!({"user":[]}),
        json!({"user":{}}),
        json!({"authMode":"token"}),
        json!({"authMode":"token","token":"   "}),
        json!({"authMode":"token","token":42}),
        json!({"authMode":"oidc","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"http://id.example.com","client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com?query=1","client_id":"client"}),
        json!({"authMode":"oidc","issuer":42,"client_id":"client"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"   "}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":42}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":"openid"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","scopes":["openid","invalid scope"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":65536}),
    ];
    for op in ["setup_save_hub", "save_hub"] {
        for fields in &invalid {
            let mut cfg = json!({"url":"local://","auth_mode":"none","set_default":true});
            cfg.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let action = if op == "setup_save_hub" {
                cfg["op"] = json!(op);
                cfg["name"] = json!("work");
                cfg
            } else {
                json!({"op":op,"name":"work","config":cfg})
            };
            let (status, response) = f.action(action.clone());
            assert_eq!(status, 400, "{action}: {response}");
            assert!(
                response["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("INVALID_INPUT:"),
                "{response}"
            );
            assert_eq!(fs::read(&f.config).unwrap(), before, "{action}");
            let (status, snapshot) = f.action(json!({"op":"get_snapshot"}));
            assert_eq!(status, 200, "{snapshot}");
            assert_eq!(snapshot["hub"], "local");
            let inbox = f.tool("cito_inbox", json!({}), human());
            assert_eq!(inbox["signals"], json!([]));
            assert_eq!(inbox["acknowledged_count"], 0);
        }
    }
    for user in [
        Value::Null,
        json!("a"),
        json!("Alice.Smith_1-"),
        json!("a".repeat(64)),
    ] {
        let (status, response) = f.action(
            json!({"op":"save_hub","name":"work","url":"local://","auth_mode":"none","user":user}),
        );
        assert_eq!(status, 200, "{response}");
        let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        assert_eq!(config["hubs"]["work"]["user"], user);
    }
    for fields in [
        json!({"authMode":"token","token":"test-token"}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","client_id":"client","scopes":["openid","profile"]}),
        json!({"authMode":"oidc","issuer":"https://id.example.com","clientId":"client","redirectPort":0}),
        json!({"authMode":"oidc","oidcIssuer":"https://id.example.com","oidcClientId":"client","oidcScopes":["openid"]}),
    ] {
        let mut cfg = fields.clone();
        cfg["url"] = json!("local://");
        cfg["enabled"] = json!(false);
        let (status, response) =
            f.action(json!({"op":"setup_save_hub","name":"work","config":cfg}));
        assert_eq!(status, 200, "{response}");
        let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        for (key, value) in fields.as_object().unwrap() {
            assert_eq!(&config["hubs"]["work"][key], value);
        }
        let (ok, out, err) = f.command(NATIVE, &["setup", "--status", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let inspection: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspection["hubs"]["work"]["authMode"], fields["authMode"]);
        assert_eq!(inspection["hubs"]["work"]["enabled"], false);
        let (ok, out, err) = f.command(NATIVE, &["find", "--user", "absent", "--json"], "");
        assert!(ok, "{fields}: {err}");
        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(found["ok"], true);
        assert_eq!(found["listeners"], json!([]));
    }
    f.stop(daemon);
    f.daemon(&[]);
    assert_eq!(
        f.action(json!({"op":"set_desktop_banner","enabled":false}))
            .0,
        200
    );
    let config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    assert_eq!(config["desktopBannerEnabled"], false);
}
#[test]
fn stdio_signal_handlers_exit_cleanly_and_daemon_health_owns_sandbox() {
    let _lock = serial();
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new(json!({}));
        f.daemon(&[]);
        let p = f.spawn(NATIVE, &["mcp-stdio"], &[]);
        f.send(p,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}));
        wait(|| !f.frames(p).is_empty());
        f.signal(p, signal);
        wait(|| f.exited(p));
        f.stop(p);
        assert_eq!(http(f.port, "/health", None).1["stateDir"], json!(f.state));
    }
}
#[test]
fn cli_validation_dry_run_and_selective_harness_configuration() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let legacy = f.state.join("plugins/pi");
    fs::create_dir_all(&legacy).unwrap();
    let manifest = json!({"id":"pi","kind":"agent","displayName":"Legacy copy",
        "version":"0.20.3","command":["./bin/cito-plugin-pi"]})
    .to_string();
    fs::write(legacy.join("plugin.json"), &manifest).unwrap();
    fs::write(legacy.join("keep"), "legacy payload").unwrap();
    let before = fs::read(&f.config).unwrap();
    for args in [
        vec!["setup", "--bogus"],
        vec!["setup", "--enable", "missing"],
        vec!["setup", "--hub", "work", "--url", "invalid"],
        vec!["setup", "--enable-hub", "missing"],
        vec!["setup", "--share-port", "invalid"],
    ] {
        let (ok, out, err) = f.command(NATIVE, &args, "");
        assert!(!ok, "{args:?}: {out} {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(legacy.join("keep")).unwrap(),
            "legacy payload"
        );
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    for args in [
        vec![
            "setup",
            "--enable",
            "pi",
            "--dry-run",
            "--json",
            "--no-login-item",
        ],
        vec!["setup", "--dry-run", "--build-app", "--json"],
        vec!["setup", "--status", "--json"],
        vec!["setup", "--status", "--build-app", "--json"],
    ] {
        let (ok, out, err) = f.command(NATIVE, &args, "");
        assert!(ok, "{args:?}: {out} {err}");
        assert_eq!(
            fs::read_to_string(legacy.join("plugin.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read_to_string(legacy.join("keep")).unwrap(),
            "legacy payload"
        );
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    assert!(!f.home.join(".pi").exists());
    fs::write(&f.config, "{\"daemonPort\":0}").unwrap();
    let (ok, out, err) = f.command(NATIVE, &["setup", "--share-hub", "--json"], "");
    assert!(ok, "{out} {err}");
    let resulting_config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    assert_eq!(resulting_config["daemonPort"], 0);
    assert_eq!(resulting_config["host_hub"]["enabled"], true);
    assert_eq!(
        fs::read_to_string(legacy.join("plugin.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(legacy.join("keep")).unwrap(),
        "legacy payload"
    );
    fs::write(&f.config, &before).unwrap();
    assert!(
        f.command(
            NATIVE,
            &["setup", "--enable", "pi", "--json", "--no-login-item"],
            ""
        )
        .0
    );
    assert!(!legacy.exists());
    assert!(f.home.join(".pi/agent/settings.json").exists());
    assert!(!f.home.join(".codex/config.toml").exists());
    assert!(!f.home.join(".claude/settings.json").exists());
    assert_eq!(
        f.command(
            f.state.join("bin/cito").to_str().unwrap(),
            &["--version"],
            ""
        )
        .1
        .trim(),
        format!("cito v{}", env!("CARGO_PKG_VERSION"))
    );
    for args in [
        vec!["listen", "--session", "orphan"],
        vec!["listen", "--harness", "missing"],
    ] {
        assert!(!f.command(NATIVE, &args, "").0);
    }
}
