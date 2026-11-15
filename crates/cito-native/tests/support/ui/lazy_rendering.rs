use super::*;

/// Observe the UI without invoking the inspection/render hooks that flush views.
fn report(f: &Fixture, p: usize, command: Value, prefix: &str) -> Value {
    let count = f
        .output(p, true)
        .lines()
        .filter(|l| l.starts_with(prefix))
        .count();
    f.send(p, command);
    wait(|| {
        f.output(p, true)
            .lines()
            .filter(|l| l.starts_with(prefix))
            .count()
            > count
    });
    serde_json::from_str(
        f.output(p, true)
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix(prefix))
            .unwrap(),
    )
    .unwrap()
}

fn render_state(f: &Fixture, p: usize) -> Value {
    report(
        f,
        p,
        json!({"op":"test_render_state"}),
        "cito-ui: render-state ",
    )
}

#[test]
fn menu_visibility_defers_views_but_status_updates_and_visibility_ipc_are_immediate() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    f.send(p, frame.clone());
    let initial = render_state(&f, p);
    assert_eq!(initial["menu_rebuilds"], 0);
    assert_eq!(initial["menu_dirty"], true);
    assert_eq!(initial["listeners"], 1);
    let active_image = initial["status_image"].clone();
    assert_ne!(active_image, "");

    frame["listeners"][0]["title"] = json!("Updated title");
    f.send(p, frame.clone());
    let title_only = render_state(&f, p);
    assert_eq!(title_only["status_updates"], initial["status_updates"]);
    assert_eq!(title_only["menu_rebuilds"], 0);
    frame["unread_count"] = json!(3);
    f.send(p, frame.clone());
    let unread = render_state(&f, p);
    assert_eq!(unread["unread_count"], 3);
    assert!(unread["button_tooltip"]
        .as_str()
        .unwrap()
        .contains("3 unread"));
    assert_eq!(
        unread["status_updates"].as_u64(),
        initial["status_updates"].as_u64().map(|n| n + 1)
    );
    assert_eq!(unread["status_image"], active_image);
    f.send(p, frame.clone());
    assert_eq!(
        render_state(&f, p)["status_updates"],
        unread["status_updates"]
    );

    f.send(p, json!({"op":"test_menu_open"}));
    let opened = render_state(&f, p);
    assert_eq!(opened["menu_open"], true);
    assert_eq!(opened["menu_dirty"], false);
    assert_eq!(opened["menu_rebuilds"], 1);
    assert_eq!(f.frames(p).last().unwrap()["op"], "open_menu");
    frame["listeners"][0]["title"] = json!("Visible title");
    f.send(p, frame.clone());
    assert_eq!(render_state(&f, p)["menu_rebuilds"], 2);
    f.send(p, json!({"op":"test_menu_close"}));
    let closed = render_state(&f, p);
    assert_eq!(closed["menu_open"], false);
    assert_eq!(f.frames(p).last().unwrap()["op"], "close_menu");
    frame["listeners"][0]["title"] = json!("Latest hidden title");
    f.send(p, frame.clone());
    assert_eq!(render_state(&f, p)["menu_rebuilds"], 2);
    let snapshot = inspect(&f, p);
    assert_eq!(snapshot["rebuilds"], 3);
    assert!(nodes(&snapshot["items"])
        .iter()
        .any(|n| n["text"] == "Latest hidden title"));
    assert_eq!(render_state(&f, p)["menu_open"], false);
    assert_eq!(
        f.frames(p)
            .iter()
            .map(|v| v["op"].clone())
            .collect::<Vec<_>>(),
        vec![json!("open_menu"), json!("close_menu")]
    );

    // Tooltip-only changes must refresh even with the same icon/count.
    let mut second = listener();
    second["session_id"] = json!("s2");
    second["scoped_id"] = json!("personal:second");
    second["listener_id"] = json!("second");
    frame["listeners"].as_array_mut().unwrap().push(second);
    f.send(p, frame.clone());
    let tooltip = render_state(&f, p);
    assert!(tooltip["button_tooltip"]
        .as_str()
        .unwrap()
        .contains("2 sessions"));
    assert_eq!(tooltip["status_image"], active_image);
    assert_eq!(
        tooltip["status_updates"].as_u64(),
        unread["status_updates"].as_u64().map(|n| n + 1)
    );

    // Returning through other states reuses the exact static image object.
    frame["hubs"][0]["status"] = json!("unreachable");
    f.send(p, frame.clone());
    let warning = render_state(&f, p);
    assert_ne!(warning["status_image"], active_image);
    frame["hubs"][0]["status"] = json!("connected");
    f.send(p, frame.clone());
    assert_eq!(render_state(&f, p)["status_image"], active_image);
    frame["hubs"][0]["status"] = json!("unreachable");
    f.send(p, frame);
    assert_eq!(render_state(&f, p)["status_image"], warning["status_image"]);
}

#[test]
fn closing_inline_rename_dirties_the_cached_menu_without_a_hidden_rebuild() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
    f.send(p, json!({"op":"test_menu_open"}));
    control(&f, p, "rename:personal:repo-abcdefghijkl");
    assert!(!node(
        &inspect(&f, p),
        "items",
        "rename_field:personal:repo-abcdefghijkl"
    )
    .is_null());
    let editing = render_state(&f, p);
    f.send(p, json!({"op":"test_menu_close"}));
    let closed = render_state(&f, p);
    assert_eq!(closed["menu_dirty"], true);
    assert_eq!(closed["menu_rebuilds"], editing["menu_rebuilds"]);
    f.send(p, json!({"op":"test_menu_open"}));
    let reopened = inspect(&f, p);
    assert!(node(
        &reopened,
        "items",
        "rename_field:personal:repo-abcdefghijkl"
    )
    .is_null());
    assert!(!node(&reopened, "items", "rename:personal:repo-abcdefghijkl").is_null());
}

#[test]
fn hidden_panels_retain_models_and_flush_only_on_open_inspect_or_capture() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let mut cfg = settings();
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    let cold = render_state(&f, p);
    assert_eq!(cold["settings_rebuilds"], 0);
    assert_eq!(cold["directory_reloads"], 0);
    f.send(p, json!({"op":"test_open_panel","surface":"settings"}));
    f.send(p, json!({"op":"test_open_panel","surface":"directory"}));
    let opened = render_state(&f, p);
    assert_eq!(opened["settings_rebuilds"], 1);
    assert_eq!(opened["directory_reloads"], 1);
    control(&f, p, "close_settings");
    control(&f, p, "close_directory");
    let closed = render_state(&f, p);
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "open_directory")
        .unwrap();
    let mut directory =
        json!({"op":"directory","peers":[peer("deferred")],"matched_count":1,"online_count":7});
    for key in ["request_id", "query", "scope", "harness"] {
        directory[key] = request[key].clone();
    }
    cfg["launch_at_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    f.send(p, directory.clone());
    let hidden = render_state(&f, p);
    assert_eq!(hidden["settings_launch_at_login"], true);
    assert_eq!(hidden["directory_peers"], 1);
    for key in ["settings_rebuilds", "directory_reloads"] {
        assert_eq!(hidden[key], closed[key], "{key}");
    }
    for key in ["settings_dirty", "directory_dirty"] {
        assert_eq!(hidden[key], true, "{key}");
    }
    f.send(p, json!({"op":"test_open_panel","surface":"settings"}));
    f.send(p, json!({"op":"test_open_panel","surface":"directory"}));
    let reopened = render_state(&f, p);
    for key in ["settings_rebuilds", "directory_reloads"] {
        assert_eq!(
            reopened[key].as_u64(),
            closed[key].as_u64().map(|n| n + 1),
            "{key}"
        );
    }
    assert_eq!(reopened["settings_dirty"], false);
    assert_eq!(reopened["directory_dirty"], false);
    let snapshot = inspect(&f, p);
    assert_eq!(
        node(&snapshot, "directory_items", "dir_title:personal:deferred")["text"],
        "deferred"
    );
    control(&f, p, "close_settings");
    control(&f, p, "close_directory");
    cfg["launch_at_login"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    // The active request changed when the panel reopened.
    let request = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "open_directory")
        .unwrap();
    directory["request_id"] = request["request_id"].clone();
    directory["peers"][0]["title"] = json!("Inspection title");
    f.send(p, directory.clone());
    let before_inspect = render_state(&f, p);
    let inspected = inspect(&f, p);
    assert_eq!(
        node(&inspected, "directory_items", "dir_title:personal:deferred")["text"],
        "Inspection title"
    );
    let flushed = render_state(&f, p);
    assert_eq!(flushed["settings_open"], false);
    assert_eq!(flushed["directory_open"], false);
    for key in ["settings_rebuilds", "directory_reloads"] {
        assert_eq!(
            flushed[key].as_u64(),
            before_inspect[key].as_u64().map(|n| n + 1),
            "{key}"
        );
    }
    assert_eq!(
        render_state(&f, p)["settings_rebuilds"],
        flushed["settings_rebuilds"]
    );

    directory["peers"][0]["title"] = json!("Captured title");
    f.send(p, directory);
    let before_capture = render_state(&f, p);
    assert_eq!(before_capture["directory_dirty"], true);
    let path = f.home.join("lazy-directory.png");
    f.send(
        p,
        json!({"op":"test_render_png","surface":"directory","path":path}),
    );
    let captured = render_state(&f, p);
    assert_eq!(
        captured["directory_reloads"].as_u64(),
        before_capture["directory_reloads"].as_u64().map(|n| n + 1)
    );
    assert_eq!(captured["directory_open"], false);
    assert_eq!(&fs::read(path).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn panel_hover_uses_exit_and_resign_events_and_menu_fallback_is_tracking_only() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    f.send(p, frame);
    f.send(p, json!({"op":"test_open_panel","surface":"settings"}));
    f.send(p, json!({"op":"test_open_panel","surface":"directory"}));
    let panels = render_state(&f, p);
    assert_eq!(panels["panel_effect_states"], json!([0, 0]));
    for (surface, id) in [
        ("settings", "close_settings"),
        ("directory", "close_directory"),
    ] {
        f.send(p, json!({"op":"test_hover","id":id,"hovered":true}));
        let hovered = render_state(&f, p);
        assert_eq!(hovered["hovered"], id);
        assert_eq!(hovered["hover_watch_active"], false);
        f.send(p, json!({"op":"test_panel_resign_key","surface":surface}));
        assert_eq!(render_state(&f, p)["hovered"], "");
        f.send(p, json!({"op":"test_hover","id":id,"hovered":true}));
        f.send(p, json!({"op":"test_hover_exit"}));
        assert_eq!(render_state(&f, p)["hovered"], "");
    }
    f.send(p, json!({"op":"test_menu_open"}));
    f.send(p, json!({"op":"test_hover","id":"quit","hovered":true}));
    assert_eq!(render_state(&f, p)["hover_watch_active"], true);
    f.send(
        p,
        json!({"op":"test_hover","id":"close_settings","hovered":true}),
    );
    assert_eq!(render_state(&f, p)["hover_watch_active"], false);
    f.send(p, json!({"op":"test_hover","id":"quit","hovered":true}));
    f.send(p, json!({"op":"test_menu_close"}));
    let closed = render_state(&f, p);
    assert_eq!(closed["hover_watch_active"], false);
    assert_eq!(closed["hovered"], "");
}

#[test]
fn cached_dates_and_fast_text_width_match_appkit_reference_metrics() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let metrics = report(
        &f,
        p,
        json!({"op":"test_text_metrics",
            "strings":["", "Stop", "Stop All", "Settings…", "Pause Inbox", "alice/fixture#abcd", "café", "中英宽度", "😀", "  spaced  ", "line\nbreak", "é"],
            "timestamps":["2000-01-01T00:00:00.125Z", "2000-01-01T00:00:00Z", "2030-01-01T01:30:00.500+01:30", "2030-01-01T00:00:00Z", "invalid", "2999-01-01T00:00:00Z", "2000-01-01T00:00:00.999Z"]
        }),
        "cito-ui: text-metrics ",
    );
    for row in metrics["widths"].as_array().unwrap() {
        assert_eq!(row["actual"], row["expected"], "{row}");
    }
    for row in metrics["dates"].as_array().unwrap() {
        assert_eq!(row["actual"], row["expected"], "{row}");
    }
    assert_eq!(metrics["dates"][4]["relative"], "Just now");
    assert_eq!(metrics["dates"][5]["relative"], "Just now");
    assert!(metrics["dates"][0]["relative"]
        .as_str()
        .unwrap()
        .ends_with("d ago"));
}
