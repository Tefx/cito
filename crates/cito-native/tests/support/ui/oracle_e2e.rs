use super::settlement::{latest, open_settings};
use super::*;

#[test]
fn pair_code_reveals_the_whole_card_from_either_scroll_direction() {
    assert_pair_code_reveal(
        "Send /start cito-abc123def4 to your account provider.",
        false,
    );
}

#[test]
fn pair_code_reveals_instructions_and_copy_on_an_oversized_card() {
    assert_pair_code_reveal(&oversized_pairing_instruction(), true);
}

fn oversized_pairing_instruction() -> String {
    let steps = (1..=19)
        .map(|i| format!("Step {i}: Follow the prompts in your account provider."))
        .collect::<Vec<_>>()
        .join("\n");
    format!("Send /start cito-abc123def4 to your account provider.\n{steps}")
}

#[test]
fn change_token_reveals_gateway_editor_over_an_oversized_pairing_receipt() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let cfg = settings();
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "pair_channel:channel");
    let request = latest(&f, p, "channel_pair");
    let instruction = oversized_pairing_instruction();
    f.send(
        p,
        json!({"op":"pair_code","source_op":"channel_pair","hub":request["hub"],
        "channel_id":request["channel_id"],"request_id":request["request_id"],
        "instruction":instruction}),
    );
    assert_eq!(
        inspect(&f, p).settings("pairing_instruction:channel")["text"],
        instruction
    );
    f.send(p, json!({"op":"test_settings_scroll","offset":100_000}));
    let before = inspect(&f, p);
    let number = |node: &Value, key: &str| node[key].as_f64().unwrap();
    assert!(
        number(&before.settings("settings_channel:channel"), "height")
            > number(&before.settings("settings_channels_tab"), "height")
    );
    assert_eq!(
        before.settings("pairing_instruction:channel")["text"],
        instruction
    );
    assert!(before.settings("channel_field:channel:secret").is_null());
    let change = "edit_hub_service:channel";
    assert_eq!(before.settings(change)["enabled"], true);
    assert_channel_view_visible(&before, "stratum_hub:channel", change);

    control(&f, p, change);
    let after = inspect(&f, p);
    assert_eq!(
        after.settings("pairing_instruction:channel")["text"],
        instruction
    );
    for id in ["channel_field:channel:secret", "save_hub_service:channel"] {
        assert_channel_view_visible(&after, "stratum_hub:channel", id);
    }
}

#[test]
fn manual_disclosure_keeps_pairing_actions_visible_on_collapse() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    open_settings(&f, p, &cfg, "channels");
    control(&f, p, "pair_channel:channel");
    let request = latest(&f, p, "channel_pair");
    assert!(request["request_id"].is_string());
    let instruction = oversized_pairing_instruction();
    f.send(
        p,
        json!({"op":"pair_code","source_op":"channel_pair","hub":request["hub"],
            "channel_id":request["channel_id"],"request_id":request["request_id"],
            "instruction":instruction,"pairing_command":"/start cito-abc123def4"}),
    );

    let manual = "manual_channel:channel";
    let user = "stratum_user:channel";
    let field = "channel_field:channel:account";
    let save = "save_channel:channel";
    let before = inspect(&f, p);
    assert_eq!(
        before.settings("pairing_instruction:channel")["text"],
        instruction
    );
    assert!(
        before.settings("settings_channel:channel")["height"]
            .as_f64()
            .unwrap()
            > before.settings("settings_channels_tab")["height"]
                .as_f64()
                .unwrap()
    );
    assert!(before.settings(field).is_null());
    assert!(before.settings(save).is_null());
    assert_channel_view_visible(&before, user, manual);

    control(&f, p, manual);
    let expanded = inspect(&f, p);
    for id in [field, save, manual] {
        assert_channel_view_visible(&expanded, user, id);
    }

    control(&f, p, manual);
    let collapsed = inspect(&f, p);
    assert!(collapsed.settings(field).is_null());
    assert!(collapsed.settings(save).is_null());
    for id in [manual, "copy_pair_command:channel"] {
        assert_eq!(collapsed.settings(id)["enabled"], true);
        assert_channel_view_visible(&collapsed, user, id);
    }
}

#[track_caller]
fn assert_channel_view_visible(s: &Value, parent: &str, id: &str) {
    let number = |node: &Value, key: &str| node[key].as_f64().unwrap();
    let clip = s.settings("settings_channels_tab");
    let card = s.settings("settings_channel:channel");
    let container = s.settings(parent);
    let view = s.settings(id);
    for (node, name) in [
        (&clip, "channels tab"),
        (&card, "channel card"),
        (&container, parent),
        (&view, id),
    ] {
        assert_eq!(node["hidden"], false, "{name} must be mounted and visible");
    }
    for (axis, origin, length) in [("x", "content_x", "width"), ("y", "content_y", "height")] {
        let start = number(&card, axis) + number(&container, axis) + number(&view, axis);
        let end = start + number(&view, length);
        let clip_start = number(&clip, origin);
        let clip_end = clip_start + number(&clip, length);
        assert!(
            clip_start <= start && end <= clip_end,
            "{id} is clipped on {axis}: {start}..{end}, viewport {clip_start}..{clip_end}"
        );
    }
}

fn assert_pair_code_reveal(instruction: &str, oversized: bool) {
    let _lock = serial();
    let mut cfg = settings();
    let target = cfg["channels"][0].clone();
    cfg["channels"] = json!((0..7)
        .map(|i| {
            let mut channel = target.clone();
            if i != 3 {
                channel["id"] = json!(format!("neighbor-{i}"));
                channel["configured"] = json!(true);
                channel["status"] = json!("ready");
            }
            channel
        })
        .collect::<Vec<_>>());
    let number = |node: &Value, key: &str| node[key].as_f64().unwrap();
    let viewport = |s: &Value| {
        let scroll = s.settings("settings_channels_tab");
        let bottom = number(&scroll, "content_y");
        (bottom, bottom + number(&scroll, "height"))
    };
    for (direction, offset) in [("above", 100_000.0), ("below", 0.0)] {
        let (f, p) = ui_fixture();
        open_settings(&f, p, &cfg, "channels");
        let scroll_to = |offset| f.send(p, json!({"op":"test_settings_scroll","offset":offset}));
        let card = inspect(&f, p).settings("settings_channel:channel");
        let card_y = number(&card, "y");
        scroll_to(card_y);
        control(&f, p, "pair_channel:channel");
        let request = latest(&f, p, "channel_pair");
        assert!(request["request_id"].is_string());
        let receipt = json!({"op":"pair_code","source_op":"channel_pair","hub":request["hub"],
            "channel_id":request["channel_id"],"request_id":request["request_id"],
            "instruction":instruction,
            "pairing_command":"/start cito-abc123def4"});

        scroll_to(offset);
        let before = inspect(&f, p);
        let clip = viewport(&before);
        assert!(
            if direction == "above" {
                card_y + number(&card, "height") <= clip.0
            } else {
                clip.1 <= card_y
            },
            "Fixture must start with the viewport {direction} the card: {clip:?}, card y={card_y}"
        );
        for (key, value) in [
            ("request_id", "unrelated-request"),
            ("hub", "work"),
            ("channel_id", "neighbor-2"),
        ] {
            assert_ne!(receipt[key], value);
            let mut mismatched = receipt.clone();
            mismatched[key] = json!(value);
            f.send(p, mismatched);
            let ignored = inspect(&f, p);
            assert_eq!(
                viewport(&ignored),
                clip,
                "Receipt with mismatched {key} scrolled from {direction}"
            );
            assert!(ignored.settings("pairing_instruction:channel").is_null());
        }

        f.send(p, receipt);
        let revealed = inspect(&f, p);
        let (bottom, top) = viewport(&revealed);
        let card = revealed.settings("settings_channel:channel");
        let card_y = number(&card, "y");
        let card_height = number(&card, "height");
        assert_eq!(
            card_height > top - bottom,
            oversized,
            "Unexpected card size: card height {card_height}, viewport height {}",
            top - bottom
        );
        if !oversized {
            assert!(
                card_y >= bottom && card_y + card_height <= top,
                "Receipt from {direction} must reveal the entire card: viewport {bottom}..{top}, card {card_y}..{}",
                card_y + card_height
            );
        }
        let user_y = card_y + number(&revealed.settings("stratum_user:channel"), "y");
        let instructions = revealed.settings("pairing_instruction:channel");
        assert_eq!(instructions["text"], instruction);
        let copy = revealed.settings("copy_pair_command:channel");
        assert_eq!(copy["enabled"], true);
        for view in [instructions, copy] {
            let y = user_y + number(&view, "y");
            assert_eq!(view["hidden"], false);
            assert!(
                y >= bottom && y + number(&view, "height") <= top,
                "{} is clipped after pairing from {direction}: viewport {bottom}..{top}, view {y}..{}",
                view["id"],
                y + number(&view, "height")
            );
        }
    }
}

#[test]
fn panel_presentation_restores_missing_focus_and_preserves_an_active_field_editor() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "hubs");
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"settings_hub_name","value":"work"}),
    );
    assert_eq!(inspect(&f, p).settings("settings_hub_name")["text"], "work");
    let prefix = "cito-ui: present-panel-focus ";
    for (i, preserve_editor) in [false, true].into_iter().enumerate() {
        f.send(
            p,
            json!({"op":"test_present_panel_focus","preserve_editor":preserve_editor}),
        );
        wait(|| {
            f.output(p, true)
                .lines()
                .filter(|line| line.starts_with(prefix))
                .count()
                > i
        });
        let output = f.output(p, true);
        let observed = output
            .lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .nth(i)
            .unwrap();
        assert_eq!(
            observed,
            format!("{preserve_editor} true true true"),
            "Expected initial focus, prepared responder, and deferred focus postcondition"
        );
    }
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_name"}));
    f.send(p, json!({"op":"test_clipboard","text":"draft"}));
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-v","via":"panel"}),
    );
    let before = inspect(&f, p);
    assert_eq!(before["field_editors"][0]["can_undo"], true);
    f.send(
        p,
        json!({"op":"test_editor_marked","surface":"settings","value":"中"}),
    );
    let composing = inspect(&f, p);
    assert_eq!(composing["field_editors"][0]["marked"], true);
    f.send(p, json!({"op":"test_open_panel","surface":"settings"}));
    f.send(
        p,
        json!({"op":"error","source_op":"open_settings","error":"Unable to refresh settings."}),
    );
    let reopened = inspect(&f, p);
    for key in [
        "instance",
        "id",
        "attached",
        "can_undo",
        "marked",
        "selection_location",
        "selection_length",
    ] {
        assert_eq!(
            reopened["field_editors"][0][key], composing["field_editors"][0][key],
            "{key}"
        );
    }
    assert_eq!(
        reopened.settings("settings_hub_name")["instance"],
        composing.settings("settings_hub_name")["instance"]
    );
}

#[test]
fn saved_panel_placement_is_constrained_and_reopen_preserves_editing() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "hubs");
    control(&f, p, "open_add_hub");
    for (index, surface, field) in [
        (0, "settings", "settings_hub_url"),
        (1, "directory", "directory_search"),
    ] {
        if surface == "directory" {
            control(&f, p, "open_directory");
        }
        for (coverage, expected) in [
            ("full", "true true true true"),
            ("partial", "true true true false"),
            ("none", "true false true false"),
        ] {
            f.send(
                p,
                json!({"op":"test_restore_placement","surface":surface,"coverage":coverage}),
            );
            inspect(&f, p);
            let output = f.output(p, true);
            assert_eq!(
                output
                    .lines()
                    .filter_map(|line| line.strip_prefix("cito-ui: restore-placement "))
                    .next_back(),
                Some(expected),
                "{surface}: {coverage}"
            );
        }
        f.send(p, json!({"op":"test_focus_field","id":field}));
        f.send(p, json!({"op":"test_clipboard","text":"draft"}));
        let op = if surface == "settings" {
            "test_settings_key"
        } else {
            "test_directory_key"
        };
        f.send(p, json!({"op":op,"key":"command-v","via":"panel"}));
        f.send(
            p,
            json!({"op":"test_editor_marked","surface":surface,"value":"中"}),
        );
        let find_editor = |snapshot: &Value| {
            snapshot["field_editors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|editor| editor["id"] == field)
                .unwrap()
                .clone()
        };
        let before = inspect(&f, p);
        let editor = find_editor(&before);
        assert_eq!(editor["marked"], true);
        assert_eq!(editor["can_undo"], true);
        f.send(p, json!({"op":"test_open_panel","surface":surface}));
        let reopened = inspect(&f, p);
        assert_eq!(
            reopened["window_frames"][index],
            before["window_frames"][index]
        );
        for key in [
            "instance",
            "attached",
            "marked",
            "can_undo",
            "selection_location",
            "selection_length",
        ] {
            assert_eq!(find_editor(&reopened)[key], editor[key], "{surface}: {key}");
        }
    }
}

#[test]
fn scroller_style_retiles_expanded_oidc_while_composing() {
    assert_settings_retile("hubs", "settings_hub_url");
}

#[test]
fn scroller_style_retiles_channel_strata_and_inbox_while_composing() {
    assert_settings_retile("channels", "channel_field:channel:account");
}

fn assert_settings_retile(tab: &str, field: &str) {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["hubs"][0]["supports_workspace_scope"] = json!(true);
    cfg["hubs"][0]["human_can_set_scope"] = json!(true);
    cfg["hubs"][0]["human_listener_id"] = json!("c_alice");
    open_settings(&f, p, &cfg, tab);
    if tab == "hubs" {
        control(&f, p, "open_add_hub");
        control(&f, p, "settings_hub_auth:oidc");
        control(&f, p, "manual_hub_oidc");
        f.send(p, json!({"op":"test_end_editing"}));
    } else {
        control(&f, p, "edit_hub_service:channel");
    }
    let number = |view: &Value, key: &str| view[key].as_f64().unwrap();
    let right = |view: &Value| number(view, "x") + number(view, "width");
    f.send(p, json!({"op":"test_tile_scroll_views","style":"overlay"}));
    f.send(p, json!({"op":"test_focus_field","id":field}));
    f.send(
        p,
        json!({"op":"test_clipboard","text":"http://example.com"}),
    );
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-v","via":"panel"}),
    );
    f.send(p, json!({"op":"test_editor_marked","value":"中"}));
    let before = inspect(&f, p);
    let editor = &before["field_editors"][0];
    assert_eq!(editor["id"], field);
    assert_eq!(editor["marked"], true);
    assert_eq!(editor["attached"], true);
    assert_eq!(editor["can_undo"], true);
    let scroll_id = format!("settings_{tab}_tab");
    let overlay = number(&before.settings(&scroll_id), "content_width");
    let rebuilds = super::lazy_rendering::render_state(&f, p)["settings_rebuilds"].clone();
    for (style, native_style, resize) in [
        ("legacy", 0, None),
        ("overlay", 1, None),
        ("overlay", 1, Some(overlay - 11.0)),
        ("overlay", 1, Some(overlay)),
    ] {
        if let Some(width) = resize {
            f.send(p, json!({"op":"test_resize_settings_scroll","width":width}));
        } else {
            f.send(p, json!({"op":"test_tile_scroll_views","style":style}));
        }
        let after = inspect(&f, p);
        for key in [
            "id",
            "instance",
            "attached",
            "marked",
            "can_undo",
            "selection_location",
            "selection_length",
        ] {
            assert_eq!(
                after["field_editors"][0][key], editor[key],
                "{style}: {key}"
            );
        }
        let scroll = after.settings(&scroll_id);
        let width = number(&scroll, "content_width");
        assert_eq!(scroll["scroller_style"], native_style);
        if style == "legacy" {
            assert!(width < overlay);
        } else {
            assert_eq!(width, resize.unwrap_or(overlay));
        }
        assert_eq!(scroll["document_width"], width);
        let mounted_field = after.settings(field);
        assert_eq!(
            mounted_field["instance"],
            before.settings(field)["instance"]
        );
        let cards = if tab == "hubs" {
            ["add_hub_form", "host_hub_card"]
        } else {
            ["settings_channel:channel", "inbox_control_bar"]
        };
        for id in cards {
            assert_eq!(after.settings(id)["width"], width - 32.0, "{style}: {id}");
        }
        if tab == "hubs" {
            assert_eq!(mounted_field["width"], width - 214.0);
            for suffix in ["", "_label"] {
                let left = after.settings(&format!("settings_hub_client_id{suffix}"));
                let right_column = after.settings(&format!("settings_hub_redirect_port{suffix}"));
                let left_width = number(&left, "width");
                let right_width = number(&right_column, "width");
                assert!((left_width - right_width).abs() <= 1.0);
                assert_eq!(number(&right_column, "x") - right(&left), 8.0);
                assert_eq!(left_width + right_width, width - 68.0);
                assert_eq!(right(&right_column), width - 46.0);
            }
        } else {
            for id in ["stratum_hub:channel", "stratum_user:channel"] {
                let stratum = after.settings(id);
                assert_eq!(number(&stratum, "width"), width - 56.0, "{style}: {id}");
                assert_eq!(right(&stratum), width - 44.0);
            }
            assert_eq!(right(&mounted_field), width - 68.0);
            let toggle = after.settings("toggle_inbox");
            let scope = after.settings("scope:c_alice");
            assert_eq!(right(&toggle), width - 44.0);
            assert!(right(&scope) < number(&toggle, "x"));
            for id in ["inbox_control_title", "inbox_control_summary"] {
                let text = after.settings(id);
                assert_eq!(right(&text) + 10.0, number(&scope, "x"), "{style}: {id}");
            }
        }
    }
    assert_eq!(
        super::lazy_rendering::render_state(&f, p)["settings_rebuilds"],
        rebuilds
    );
}

#[test]
fn main_menu_key_equivalents_reach_the_key_windows_appkit_responder_chain() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    open_settings(&f, p, &settings(), "apps");
    f.send(
        p,
        json!({"op":"test_main_menu_shortcuts","replacement":"Replacement"}),
    );
    let snapshot = inspect_unchecked(&f, p, None);
    let output = f.output(p, true);
    assert_eq!(
        output
            .lines()
            .filter_map(|line| line.strip_prefix("cito-ui: main-menu-key "))
            .collect::<Vec<_>>(),
        [
            "a true true 8 Original",
            "c true true 8 Original",
            "v true true 0 Replacement",
            "z true true 8 Original",
            "w true true -1 Original"
        ]
    );
    assert_eq!(snapshot["settings_open"], false);
}

#[test]
fn failed_state_and_directory_receipts_route_errors_before_projection_guards() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["settings"]["hubs"][0]["human_state_status"] = json!("current");
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    for (operation, id) in [
        ("human_inbox_update", "toggle_inbox"),
        ("unlisten_all", "stop_all"),
    ] {
        control(&f, p, id);
        inspect(&f, p);
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == operation)
            .unwrap();
        f.send(p, json!({"op":"state","ok":false,"source_op":operation,"request_id":request["request_id"],"error":"Mutation failed"}));
        let s = inspect(&f, p);
        assert_eq!(s["pending_controls"], json!([]));
        let label = s.settings("settings_error_label");
        assert_eq!(label["text"], "Mutation failed");
        assert_eq!(label["wraps"], true);
        assert_eq!(label["maximum_lines"], 2);
    }
    control(&f, p, "open_directory");
    let query = latest(&f, p, "open_directory");
    for (op, source) in [
        ("directory", "open_directory"),
        ("error", "directory"),
        ("directory", "directory"),
        ("directory", ""),
    ] {
        let error = format!("Directory unavailable: {op}/{source}");
        f.send(p, json!({"op":op,"ok":false,"source_op":source,"request_id":query["request_id"],"error":error}));
        let s = inspect(&f, p);
        let warning = s.directory("directory_warning_label");
        assert_eq!(warning["hidden"], false);
        assert_eq!(
            warning["text"],
            "Cannot refresh Directory. Showing the last available results."
        );
        assert_eq!(warning["tooltip"], error);
        assert_eq!(
            s.settings("settings_error_label")["text"],
            "Mutation failed",
            "Directory failures must not replace the Settings banner"
        );
    }
}

#[test]
fn partial_settings_keep_channels_and_actions_use_saved_values_and_drafts() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    let mut cfg = settings();
    let channel = &mut cfg["channels"][0];
    channel["configured"] = json!(true);
    channel["status"] = json!("ready");
    channel["warning"] = json!(true);
    channel["fields"][1]["value"] = json!("saved-account");
    channel["fields"].as_array_mut().unwrap().push(json!({
        "key":"gateway", "scope":"hub", "label":"Gateway", "type":"string",
        "required":false, "placeholder":"Gateway", "value":"saved-gateway", "configured":true
    }));
    frame["settings"] = cfg.clone();
    f.send(p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    cfg.as_object_mut().unwrap().remove("channels");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert_eq!(s.settings("test_channel:channel")["enabled"], true);
    control(&f, p, "test_channel:channel");
    inspect(&f, p);
    let action = f.frames_for(p, "channel_test").next_back().unwrap();
    assert_eq!(
        action["values"],
        json!({"account":"saved-account","gateway":"saved-gateway"})
    );
    f.send(p, json!({"op":"channel_test_result","request_id":action["request_id"],"channel_id":"channel","ok":true}));
    control(&f, p, "edit_user_channel:channel");
    f.send(p, json!({"op":"test_settings_input","id":"channel_field:channel:account","value":"draft-account"}));
    control(&f, p, "test_channel:channel");
    inspect(&f, p);
    let action = f.frames_for(p, "channel_test").next_back().unwrap();
    assert_eq!(
        action["values"],
        json!({"account":"draft-account","gateway":"saved-gateway"})
    );
    control(&f, p, "unpair_channel:channel");
    let s = inspect(&f, p);
    let action = f.frames_for(p, "channel_unpair").next_back().unwrap();
    assert_eq!(s["pending_channel_resets"], json!([action["request_id"]]));
    f.send(
        p,
        json!({"op":"error","request_id":action["request_id"],"error":"Try again"}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["pending_channel_resets"], json!([]));
    assert_eq!(
        s.settings("channel_field:channel:account")["text"],
        "draft-account"
    );
}

#[test]
fn pin_stop_and_auto_listen_settle_through_the_real_daemon_with_semantic_a() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered");
    let mut cycle = ClosedLoop::new(&f);
    f.tool(
        "cito_listen",
        json!({"about":"Lifecycle check"}),
        meta.clone(),
    );
    let s = cycle.until(|s| !s.menu("stop_all").is_null());
    let stop_id = nodes(&s["items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("stop:"))
                .map(str::to_owned)
        })
        .unwrap();
    let pin_id = stop_id.replacen("stop:", "pin:", 1);
    let session_id = stop_id.replacen("stop:", "session:", 1);
    assert!(
        s.menu(&pin_id).is_null(),
        "Manual listening without Auto-Listen needs no pin control"
    );

    cycle.control("toggle_auto_listen");
    let s = cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin.fill");
    assert!(s.menu("toggle_auto_listen")["tooltip"]
        .as_str()
        .unwrap()
        .contains("keeps pinned sessions listening"));
    cycle.control("toggle_auto_listen");
    let s = cycle.until(|s| s.menu("toggle_auto_listen")["text"] == "Auto-Listen");
    assert!(
        !s.menu(&session_id).is_null(),
        "Turning Auto-Listen off preserves pinned listeners"
    );
    assert!(s.menu(&pin_id).is_null());

    cycle.control("toggle_auto_listen");
    cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin.fill");
    cycle.control(&pin_id);
    cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin");
    assert!(cycle
        .results
        .iter()
        .any(|(a, status, _)| a["op"] == "set_listen_mode"
            && a["listen_mode"] == "auto"
            && *status == 200));
    cycle.control(&pin_id);
    cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin.fill");
    cycle.control(&pin_id);
    cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin");
    cycle.control("toggle_auto_listen");
    cycle.until(|s| {
        s.menu(&session_id).is_null() && s.menu("toggle_auto_listen")["text"] == "Auto-Listen"
    });

    f.tool("cito_listen", json!({"about":"Stop only me"}), meta.clone());
    cycle.until(|s| !s.menu(&session_id).is_null());
    cycle.control("toggle_auto_listen");
    cycle.until(|s| s.menu(&pin_id)["symbol"] == "pin.fill");
    cycle.control(&stop_id);
    let s = cycle.until(|s| s.menu(&session_id).is_null());
    assert_eq!(s.menu("toggle_auto_listen")["text"], "Auto-Listen: On");
    assert_eq!(f.snapshot()["hubs"][0]["auto_listen"], true);

    f.tool("cito_listen", json!({"about":"Stop all agents"}), meta);
    cycle.until(|s| !s.menu("stop_all").is_null());
    cycle.control("stop_all");
    cycle.until(|s| {
        s.menu(&session_id).is_null() && s.menu("toggle_auto_listen")["text"] == "Auto-Listen"
    });
    assert_eq!(f.snapshot()["hubs"][0]["auto_listen"], false);
    let (status, _) = f.action(json!({"op":"unlisten_all","hub":"local"}));
    assert_eq!(
        status, 200,
        "Bulk stop remains idempotent with no active agents"
    );
}

#[test]
fn inbox_and_channel_diagnostics_wrap_without_displacing_or_clipping_controls() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["hubs"][0]["human_state_status"] = json!("current");
    cfg["hubs"][0]["attachments"] = json!((0..8).map(|index| json!({
        "channel_id":format!("channel-{index}"),"location":"hub",
        "display_name":format!("Alerts for the engineering workgroup {index}"),"availability":"ACTIVE","is_local":false
    })).collect::<Vec<_>>());
    let detail =
        "Please check the account and review the delivery settings before trying again. ".repeat(4);
    f.send(p, state_with_settings(cfg.clone()));
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let ready = inspect(&f, p);
    assert!(
        ready.settings("inbox_control_bar")["height"]
            .as_f64()
            .unwrap()
            > 48.0
    );
    cfg["channels"][0]["warning"] = json!(true);
    cfg["channels"][0]["detail"] = json!(detail.trim());
    f.send(p, state_with_settings(cfg));
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        let bar = s.settings("inbox_control_bar");
        let height = bar["height"].as_f64().unwrap();
        let action = s.settings("toggle_inbox");
        let summary = s.settings("inbox_control_summary");
        assert!(height > 48.0);
        assert_eq!(action["y"].as_f64().unwrap(), (height - 22.0) / 2.0);
        assert_eq!(summary["wraps"], true);
        assert!(summary["height"].as_f64().unwrap() > 24.0);
        assert!(summary["text"].as_str().unwrap().contains("workgroup 7"));
        let selector = s.settings("channel_hub_strip");
        assert!(
            selector["y"].as_f64().unwrap() + selector["height"].as_f64().unwrap()
                < bar["y"].as_f64().unwrap()
        );
        let diagnostic = s.settings("settings_channel_detail:channel");
        assert_eq!(diagnostic["wraps"], true);
        assert!(diagnostic["height"].as_f64().unwrap() > 24.0);
        assert_eq!(diagnostic["text"], detail.trim());
    }
}

#[test]
fn add_hub_errors_wrap_below_fields_and_above_actions() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let frame = state_with_settings(settings());
    send_companion(&f, p, frame);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "open_add_hub");
    f.send(p, json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://hub.example.com"}));
    control(&f, p, "submit_add_hub");
    let submitted = latest(&f, p, "setup_save_hub");
    let clean =
        "The Hub is unavailable. Check its address and network connection before trying again. "
            .repeat(3);
    send_companion(
        &f,
        p,
        json!({"op":"error","source_op":"setup_save_hub","request_id":submitted["request_id"],"error":format!("10071: {clean}")}),
    );
    let editing = inspect_unchecked(&f, p, None);
    assert_eq!(editing["field_editors"][0]["id"], "settings_hub_url");
    assert_eq!(editing["settings_rebuild_deferred"], true);
    assert_eq!(editing.settings("add_hub_error")["tooltip"], clean.trim());
    f.send(p, json!({"op":"test_end_editing"}));
    for appearance in ["light", "dark"] {
        let s = inspect_as(&f, p, Some(appearance));
        let error = s.settings("add_hub_error");
        assert_eq!(error["text"], clean.trim());
        assert_eq!(error["tooltip"], clean.trim());
        assert_eq!(
            error["width"].as_f64().unwrap(),
            s.settings("add_hub_form")["width"].as_f64().unwrap() - 28.0
        );
        assert!(error["height"].as_f64().unwrap() > 15.0);
        let submit = s.settings("submit_add_hub");
        assert!(
            submit["y"].as_f64().unwrap() + submit["height"].as_f64().unwrap()
                <= error["y"].as_f64().unwrap()
        );
    }
}

#[test]
fn segment_and_confirmation_controls_keep_pressed_label_contrast() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["hubs"][1]["name"] = json!("Work-Internal");
    let mut frame = state_with_settings(cfg);
    frame["hubs"][1]["name"] = json!("Work-Internal");
    frame["hubs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"ACTIVE","status":"connected","user":"bob","auth_mode":"none"}));
    let activity = json!([{"hub":"personal","signal_id":"review","listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Review","target_harness":"human","target_title":"Me","message":"Ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true}]);
    f.send(p, frame);
    let initial = inspect(&f, p);
    assert_eq!(initial.menu("hub:Work-Internal")["text"], "Work-Internal");
    assert_eq!(initial.menu("hub:ACTIVE")["text"], "ACTIVE");
    control(&f, p, "open_directory");
    for (id, reduce_motion) in [("dir_mode:activity", false), ("dir_mode:directory", true)] {
        let snapshot = inspect_frame(
            &f,
            p,
            json!({"op":"test_control","id":id,"reduce_motion":reduce_motion}),
        );
        let track = snapshot.directory("directory_mode_track");
        assert_eq!(track["selected_id"], id);
        assert_eq!(track["sliding"], !reduce_motion);
        assert_eq!(snapshot["clipped_views"], json!([]));
    }
    control(&f, p, "dir_mode:activity");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":activity,"activity_filter":"me","activity_total":1}),
    );
    f.send(p, json!({"op":"test_directory_key","key":"cmd-f","via":"panel","characters":"f","key_code":3}));
    assert!(inspect(&f, p)["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == "directory_search" && e["attached"] == true));
    for style in ["overlay", "legacy"] {
        f.send(p, json!({"op":"test_tile_scroll_views","style":style}));
        for appearance in ["light", "dark"] {
            inspect_as(&f, p, Some(appearance));
            for id in ["dir_mode:activity", "activity_clear_confirm_commit"] {
                if id == "activity_clear_confirm_commit" {
                    control(&f, p, "activity_clear");
                }
                let target = inspect(&f, p).directory(id);
                assert_eq!(
                    target["enabled"], true,
                    "{id} {style} {appearance}: {target}"
                );
                assert_eq!(target["hidden"], false, "{id} {style} {appearance}");
                let pressed = inspect_frame(
                    &f,
                    p,
                    json!({"op":"test_control","id":id,"drag":[false],"inspect_pressed":true,"appearance":appearance}),
                );
                let control_state = pressed.directory(id);
                assert_eq!(control_state["pressed"], true, "{id} {style} {appearance}");
                assert!(
                    nodes(&control_state["children"])
                        .iter()
                        .any(|n| n["contrast"].as_f64().is_some_and(|ratio| ratio >= 7.0)),
                    "{id} {style} {appearance}: {control_state}"
                );
                assert_eq!(pressed["low_contrast_labels"], json!([]));
                if id == "activity_clear_confirm_commit" {
                    control(&f, p, "activity_clear_confirm_cancel");
                }
            }
        }
    }
    assert_eq!(f.frames_for(p, "clear_activity_history").count(), 0);
}

#[test]
fn activity_clear_confirmation_and_dynamic_accessibility() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    let mut cfg = settings();
    cfg["apps"][0]["display_name"] = json!("Pi Coding Agent");
    cfg["apps"][2]["id"] = json!("claude");
    cfg["apps"][2]["display_name"] = json!("Claude Code");
    cfg["channels"][0]["id"] = json!("telegram");
    cfg["channels"][0]["display_name"] = json!("Telegram");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["fields"][0]["label"] = json!("Bot Token");
    cfg["channels"][0]["fields"][1]["label"] = json!("Chat ID");
    cfg["host_hub"] = json!({"enabled":true,"running":true,"port":4291,"endpoint_url":"http://192.0.2.10:4291","lan_url":"http://192.0.2.10:4291","token":"example-token","auth_mode":"token","invite_command":"cito setup --hub work --url http://192.0.2.10:4291 --default"});
    frame["settings"] = cfg.clone();
    frame["harnesses"][0]["id"] = json!("pi");
    frame["harnesses"][0]["display_name"] = json!("Pi");
    frame["listeners"][0]["harness"] = json!("pi");
    frame["listeners"][0]["display_address"] = json!("alice/pi#abcd");
    frame["listeners"][0]["title"] = json!("Release review");
    frame["listeners"][0]["listen_mode"] = json!("manual");
    frame["hubs"][0]["auto_listen"] = json!(true);
    frame["listeners"][0]["cwd"] = json!("~/work/service");
    frame["directory_online_count"] = json!(3);
    frame["hubs"][0]["human_listener_id"] = json!("human-node");
    frame["hubs"][0]["human_control_revision"] = json!(4);
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_paused"] = json!(false);
    frame["hubs"][0]["attachments"] = json!([{"channel_id":"hub:telegram","location":"hub","plugin_id":"telegram","display_name":"Telegram","availability":"ACTIVE","is_local":false},{"channel_id":"here:human","location":"device","host_id":"here","display_name":"laptop","availability":"ACTIVE","is_local":true}]);
    frame["unread_count"] = json!(2);
    frame["activity"]=json!([("review","The review is ready. Can you check the final changes?",true,true),("build","Build passed. All checks are green.",false,false),("reply","Approved. Please continue with the next step.",true,true)].into_iter().map(|(id,message,unread,for_me)|json!({"hub":"personal","signal_id":id,"listener_id":"human-node","from_user":"bob","from_host_name":"desktop","from_harness":"pi","from_title":"Release review","from_scoped_id":"personal:sender","from_display_address":"bob/pi#abcd","target_harness":if for_me{"human"}else{"pi"},"target_title":"Build session","target_scoped_id":"personal:receiver","message":message,"created_at":"2030-01-01T00:00:00Z","unread":unread,"for_me":for_me,"can_reply":for_me})).collect::<Vec<_>>());
    let activity_items = frame["activity"].clone();
    frame["activity_filter"] = json!("all");
    frame["activity_total"] = json!(3);
    f.send(p, frame);
    let s = inspect(&f, p);
    assert!(s["button_tooltip"].as_str().unwrap().contains("unread"));
    let activity = s.menu("open_activity");
    assert!(!activity["tooltip"].as_str().unwrap().is_empty());
    assert!(nodes(&activity["children"])
        .iter()
        .all(|child| child["tooltip"].as_str().unwrap_or("").is_empty()));
    control(&f, p, "open_directory");
    let mut schema = peer("schema");
    schema["harness"] = json!("pi");
    schema["display_address"] = json!("bob/pi#df0a");
    let mut review = peer("review");
    review["harness"] = json!("pi");
    review["display_address"] = json!("bob/pi#c97a");
    let mut inbox = peer("inbox");
    inbox["harness"] = json!("human");
    inbox["title"] = json!("Human");
    inbox["about"] = json!("Human");
    inbox["repo"] = Value::Null;
    inbox["branch"] = Value::Null;
    inbox["display_address"] = json!("@bob");
    inbox["attachments"] = json!([{"channel_id":"hub:telegram","location":"hub","plugin_id":"telegram","display_name":"Telegram","availability":"ACTIVE","is_local":false}]);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[schema,review,inbox],"matched_count":3,"online_count":3}),
    );
    let directory = inspect(&f, p);
    let scope = directory.directory("directory_scope_track");
    let separator = directory.directory("directory_facet_separator");
    assert!(
        separator["x"].as_f64().unwrap()
            > scope["x"].as_f64().unwrap() + scope["width"].as_f64().unwrap()
    );
    control(&f, p, "copy_dir:personal:schema");
    let copied = inspect(&f, p);
    assert_eq!(
        copied.directory("copy_dir:personal:schema")["accessibility_label"],
        "✓ Copied"
    );
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    control(&f, p, "reply_activity:review");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Approved. Please continue."}),
    );
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert!(f
        .frames(p)
        .iter()
        .any(|v| v["op"] == "ack_signals" && v["signal_ids"] == json!(["review"])));
    control(&f, p, "composer_cancel");
    inspect(&f, p);
    control(&f, p, "activity_clear");
    let confirm = inspect(&f, p);
    assert_eq!(
        confirm.directory("activity_clear_confirm_label")["text"],
        "Clear 3 All Activity messages?"
    );
    assert!(!confirm.directory("activity_clear_confirm_commit").is_null());
    control(&f, p, "activity_clear_confirm_cancel");
    inspect(&f, p);
    assert!(f.frames_for(p, "clear_activity_history").next().is_none());
    f.send(
        p,
        json!({"op":"test_directory_query","query":"Build passed"}),
    );
    let cached = inspect(&f, p);
    assert!(!cached.directory("activity_row:build").is_null());
    assert_eq!(cached.directory("activity_clear")["enabled"], false);
    control(&f, p, "activity_clear");
    assert!(inspect(&f, p)
        .directory("activity_clear_confirm_label")
        .is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[activity_items[1]],"activity_total":2,"activity_filter":"all","activity_query":"Build passed"}),
    );
    control(&f, p, "activity_clear");
    assert_eq!(
        inspect(&f, p).directory("activity_clear_confirm_label")["text"],
        "Clear 2 matching messages?"
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[],"activity_total":0,"activity_filter":"all","activity_query":"Build passed"}),
    );
    control(&f, p, "act_reset_search");
    let reset = inspect(&f, p);
    assert_eq!(reset["directory_query"], "");
    assert!(reset.directory("activity_clear_confirm_commit").is_null());
    assert_eq!(reset.directory("activity_clear")["enabled"], false);
    assert!(f.frames_for(p, "clear_activity_history").next().is_none());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":activity_items,"activity_total":3,"activity_filter":"all","activity_query":""}),
    );
    control(&f, p, "activity_clear");
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    let cancelled = inspect(&f, p);
    assert_eq!(cancelled["directory_open"], true);
    assert!(cancelled
        .directory("activity_clear_confirm_commit")
        .is_null());
    control(&f, p, "activity_clear");
    control(&f, p, "activity_clear_confirm_commit");
    inspect(&f, p);
    let cleared = f.frames_for(p, "clear_activity_history").next().unwrap();
    assert_eq!(cleared["filter"], "all");
    assert_eq!(cleared["query"], "");
}

#[test]
fn expanding_channel_gateway_reveals_the_card_action_edge() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    let mut first = cfg["channels"][0].clone();
    first["id"] = json!("other");
    first["hub_ready"] = json!(false);
    first["setup_guide"] =
        json!(["Read the service setup instructions before entering a token. ".repeat(25)]);
    cfg["channels"].as_array_mut().unwrap().insert(0, first);
    open_settings(&f, p, &cfg, "channels");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    let before = inspect(&f, p);
    assert!(
        before.settings("settings_channel:channel")["y"]
            .as_f64()
            .unwrap()
            < before.settings("settings_channels_tab")["content_y"]
                .as_f64()
                .unwrap()
    );
    control(&f, p, "edit_hub_service:channel");
    let after = inspect(&f, p);
    assert_eq!(after.settings("save_hub_service:channel")["enabled"], true);
    let viewport = after.settings("settings_channels_tab");
    let bottom = viewport["content_y"].as_f64().unwrap();
    let card_bottom = after.settings("settings_channel:channel")["y"]
        .as_f64()
        .unwrap();
    assert!(
        bottom <= card_bottom
            && card_bottom + 44.0 <= bottom + viewport["height"].as_f64().unwrap()
    );
}

#[test]
fn native_panel_navigation_accepts_ime_keys_without_editing_marked_text() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state_with_settings(settings()));
    control(&f, p, "open_settings");
    let key = |surface: &str, command: &str, characters: &str, code: u16| {
        f.send(
            p,
            json!({"op":format!("test_{surface}_key"),"key":command,"via":"panel",
            "characters":characters,"key_code":code}),
        );
        inspect(&f, p)
    };
    assert_eq!(
        key("settings", "command-3", "界", 20)["settings_tab"],
        "hubs"
    );
    control(&f, p, "open_add_hub");
    assert_eq!(inspect(&f, p)["field_editors"][0]["id"], "settings_hub_url");
    f.send(p, json!({"op":"test_clipboard","text":"must not paste"}));
    f.send(p, json!({"op":"test_editor_marked","value":"中"}));
    assert_eq!(
        key("settings", "command-v", "界", 9)["field_editors"][0]["marked"],
        true
    );
    assert_eq!(
        key("settings", "command-1", "界", 18)["settings_tab"],
        "apps"
    );
    control(&f, p, "open_uninstall_confirm");
    assert_eq!(
        key("settings", "command-2", "界", 19)["settings_tab"],
        "apps"
    );
    assert_eq!(
        key("settings", "command-k", "界", 40)["directory_open"],
        false
    );
    f.send(
        p,
        json!({"op":"test_key_event","character":"界","key_code":40,"modifiers":["command"]}),
    );
    assert_eq!(inspect(&f, p)["directory_open"], false);
    control(&f, p, "cancel_uninstall_all");
    assert_eq!(
        key("settings", "command-2", "界", 19)["settings_tab"],
        "channels"
    );
    assert_eq!(
        key("settings", "command-k", "界", 40)["directory_open"],
        true
    );
    f.send(
        p,
        json!({"op":"test_directory_query","query":"select this"}),
    );
    let directory = key("directory", "command-k", "界", 40);
    let search = directory["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|editor| editor["id"] == "directory_search")
        .unwrap();
    assert_eq!(search["selection_length"], "select this".len());
    f.send(
        p,
        json!({"op":"test_editor_marked","surface":"directory","value":"中"}),
    );
    let unchanged = key("directory", "command-3", "界", 20);
    assert!(unchanged["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|editor| editor["id"] == "directory_search" && editor["marked"] == true));
    assert_eq!(
        key("directory", "command-2", "界", 19)["directory_mode"],
        "activity"
    );
    assert_eq!(
        key("directory", "command-1", "界", 18)["directory_mode"],
        "directory"
    );
    control(&f, p, "close_settings");
    assert_eq!(
        key("directory", "command-comma", "，", u16::MAX)["settings_open"],
        true
    );
    assert_eq!(
        key("directory", "command-w", "界", 13)["directory_open"],
        false
    );
    assert_eq!(
        key("settings", "command-w", "界", 13)["settings_open"],
        false
    );
    assert_eq!(f.frames_for(p, "close_settings").count(), 2);
    assert_eq!(f.frames_for(p, "close_directory").count(), 1);
}

#[test]
fn native_edit_shortcuts_use_real_field_editors_for_secure_text_search_and_composer() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let frame = state_with_settings(settings());
    f.send(p, frame);
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-comma","via":"panel"}),
    );
    assert_eq!(
        inspect(&f, p)["settings_open"],
        false,
        "A missing panel must not fall through to the direct Settings handler"
    );
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    control(&f, p, "edit_hub_service:channel");
    control(&f, p, "open_directory");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"matched_count":1}),
    );
    let mut overlay_widths = None;
    for (style, native_style) in [("overlay", 1), ("legacy", 0), ("overlay", 1)] {
        f.send(p, json!({"op":"test_tile_scroll_views","style":style}));
        let s = inspect(&f, p);
        assert_eq!(s["clipped_views"], json!([]), "{style}");
        assert_eq!(s["overlapping_siblings"], json!([]), "{style}");
        let directory = s.directory("directory_scroll");
        let settings = s.settings("settings_channels_tab");
        assert_eq!(directory["scroller_style"], native_style);
        assert_eq!(settings["scroller_style"], native_style);
        let directory_width = directory["content_width"].as_f64().unwrap();
        let settings_width = settings["content_width"].as_f64().unwrap();
        if style == "legacy" {
            let (overlay_directory, overlay_settings) = overlay_widths.unwrap();
            assert!(directory_width < overlay_directory);
            assert!(settings_width < overlay_settings);
        } else if let Some(widths) = overlay_widths {
            assert_eq!((directory_width, settings_width), widths);
        } else {
            overlay_widths = Some((directory_width, settings_width));
        }
        let card = s.settings("settings_channel:channel");
        assert_eq!(card["x"], 16);
        assert_eq!(card["width"].as_f64().unwrap(), settings_width - 32.0);
        let table = s.directory("directory_table");
        assert_eq!(table["column_width"], directory["content_width"]);
        let message = s.directory("msg_dir:personal:one");
        assert_eq!(
            message["x"].as_f64().unwrap() + message["width"].as_f64().unwrap(),
            directory_width - 16.0
        );
        for panel in [s.settings("settings_panel"), s.directory("directory_panel")] {
            assert_eq!(panel["radius"], 0);
            assert_eq!(panel["border_width"], 0);
        }
    }
    let s = inspect(&f, p);
    assert_eq!(
        s["key_panels"],
        json!([{
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"automatic_key_view_loop":true,
            "closable":true,"miniaturizable":true,"visible_window_buttons":true,"close_enabled":true,"miniaturize_enabled":true,"zoom_disabled":true,
            "close_and_minimize_hit_test":true,"window_button_positions":[[14,24],[34,24],[54,24]]
        }, {
            "titled":true,"full_size_content":true,"hidden_title":true,"transparent_titlebar":true,"automatic_key_view_loop":true,
            "closable":true,"miniaturizable":true,"visible_window_buttons":true,"close_enabled":true,"miniaturize_enabled":true,"zoom_disabled":true,
            "close_and_minimize_hit_test":true,"window_button_positions":[[14,23],[34,23],[54,23]]
        }])
    );
    assert!(s.settings("close_settings").is_null());
    assert!(s.directory("close_directory").is_null());
    let tabs = s.settings("settings_tab_track");
    let x = tabs["x"].as_f64().unwrap();
    assert!(x >= 78.0);
    assert!((2.0 * x + tabs["width"].as_f64().unwrap() - 520.0).abs() <= 2.0);
    assert_eq!(s.directory("directory_mode_track")["x"], 78);
    let mode = s.directory("directory_mode_track");
    let search = s.directory("directory_search");
    assert!(
        search["x"].as_f64().unwrap()
            > mode["x"].as_f64().unwrap() + mode["width"].as_f64().unwrap()
    );
    assert_eq!(
        search["x"].as_f64().unwrap() + search["width"].as_f64().unwrap(),
        546.0
    );
    assert_eq!(
        json!(&s["edit_menu"].as_array().unwrap()[..6]),
        json!([
            {"title":"Undo","key":"z","action":"undo:","shift":false},
            {"title":"Redo","key":"Z","action":"redo:","shift":true},
            {"title":"Cut","key":"x","action":"cut:","shift":false},
            {"title":"Copy","key":"c","action":"copy:","shift":false},
            {"title":"Paste","key":"v","action":"paste:","shift":false},
            {"title":"Select All","key":"a","action":"selectAll:","shift":false}
        ])
    );
    let focus = |id: &str| {
        f.send(p, json!({"op":"test_focus_field","id":id}));
        assert!(
            inspect(&f, p)["field_editors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|editor| editor["id"] == id),
            "{id} must own a native field editor"
        );
    };
    let key = |op: &str, key: &str, via: &str| {
        f.send(p, json!({"op":op,"key":key,"via":via}));
        let snapshot = inspect(&f, p);
        for editor in snapshot["field_editors"].as_array().unwrap() {
            assert_eq!(editor["attached"], true, "{key}: {editor}");
            assert_eq!(editor["automatic_groups"], true, "{key}: {editor}");
        }
        snapshot
    };
    let save_secret = |expected: &str| {
        let before = f.frames(p).len();
        control(&f, p, "save_hub_service:channel");
        let mut saved = Value::Null;
        wait(|| {
            saved = f
                .frames(p)
                .into_iter()
                .skip(before)
                .find(|a| a["op"] == "save_hub_service")
                .unwrap_or(Value::Null);
            saved.is_object()
        });
        assert_eq!(saved["values"]["secret"], expected);
    };
    focus("channel_field:channel:secret");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"opaque secure value / keep spaces"}),
    );
    key("test_settings_key", "command-v", "panel");
    save_secret("opaque secure value / keep spaces");
    key("test_settings_key", "command-a", "direct");
    f.send(
        p,
        json!({"op":"test_clipboard","text":"replacement secure value + punctuation"}),
    );
    key("test_settings_key", "command-v", "panel");
    save_secret("replacement secure value + punctuation");
    key("test_settings_key", "command-z", "direct");
    save_secret("opaque secure value / keep spaces");
    key("test_settings_key", "shift-command-z", "panel");
    save_secret("replacement secure value + punctuation");
    key("test_settings_key", "command-a", "panel");
    f.send(p, json!({"op":"test_clipboard","text":"public clipboard"}));
    for command in ["command-c", "command-x"] {
        assert_eq!(
            key("test_settings_key", command, "panel")["clipboard"],
            "public clipboard"
        );
    }
    let unchanged = inspect(&f, p);
    assert_eq!(
        unchanged.settings("save_hub_service:channel")["enabled"],
        false,
        "Clipboard shortcuts cannot resubmit an unchanged pending secret"
    );
    assert_eq!(
        unchanged.settings("channel_field:channel:secret")["text"],
        "••••"
    );

    for (id, op, surface) in [
        (
            "channel_field:channel:account",
            "test_settings_key",
            "settings_items",
        ),
        ("directory_search", "test_directory_key", "directory_items"),
        ("composer_input", "test_directory_key", "directory_items"),
    ] {
        if id == "composer_input" {
            control(&f, p, "msg_dir:personal:one");
        }
        focus(id);
        let text = "café / ordinary text";
        f.send(p, json!({"op":"test_clipboard","text":"stale cached text"}));
        f.send(
            p,
            json!({"op":"test_clipboard","system_only":true,"text":text}),
        );
        assert_eq!(
            node(&key(op, "command-v", "panel"), surface, id)["text"],
            text
        );
        let selected = key(op, "command-a", "direct");
        let editor = selected["field_editors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap();
        assert_eq!(
            editor["selection_length"],
            text.encode_utf16().count(),
            "{id}: {editor}"
        );
        assert_eq!(editor["editable"], true, "{id}: {editor}");
        f.send(
            p,
            json!({"op":"test_clipboard","text":"copy must replace this sentinel"}),
        );
        let copied = key(op, "command-c", "panel");
        assert_eq!(copied["clipboard"], text);
        let editor = copied["field_editors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap();
        assert_eq!(
            editor["selection_length"],
            text.encode_utf16().count(),
            "{id}: {editor}"
        );
        assert_eq!(
            node(&key(op, "command-x", "panel"), surface, id)["text"],
            "",
            "{id}"
        );
        assert_eq!(
            node(&key(op, "command-z", "direct"), surface, id)["text"],
            text,
            "{id}"
        );
        assert_eq!(
            node(&key(op, "shift-command-z", "panel"), surface, id)["text"],
            ""
        );
    }
    f.send(
        p,
        json!({"op":"test_clipboard","text":"Final café message"}),
    );
    key("test_directory_key", "command-v", "panel");
    let before = f.frames(p).len();
    // A panel key-equivalent miss must not fall through to the direct Return handler.
    key("test_directory_key", "return", "panel");
    assert!(!f
        .frames(p)
        .iter()
        .skip(before)
        .any(|a| a["op"] == "send_signal"));
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p)
            .iter()
            .skip(before)
            .any(|a| a["op"] == "send_signal" && a["message"] == "Final café message")
    });
    assert_eq!(
        key("test_settings_key", "command-w", "panel")["settings_open"],
        false
    );
    assert_eq!(
        key("test_directory_key", "command-comma", "panel")["settings_open"],
        true
    );
}

#[test]
fn r1_r5_real_daemon_ui_closed_loop_gateway_personal_errors_and_retina_states() {
    let _lock = serial();
    let f = Fixture::new(json!({"host_hub":{"bind":"127.0.0.1","port":port()}}));
    f.provider("delivered_unconfirmed");
    let plugin = f.state.join("plugins/channel");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"),json!({"id":"channel","kind":"human-channel","scope":"hub","displayName":"Messenger","setupGuide":["Create a gateway credential with your provider.","Save it once, then pair your personal account."],
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"pairing":{"supported":true},"fields":[
        {"key":"credential","label":"Gateway Token","scope":"hub","type":"secret","required":true},
        {"key":"address","label":"Personal Address","scope":"user","type":"string","required":true}]}).to_string()).unwrap();
    let mut l = ClosedLoop::new(&f);
    l.until(|s| s.menu("header").is_object());
    l.control("open_settings");
    l.until(|s| s["settings_open"] == true);
    l.control("settings_tab:channels");
    let s = l.until(|s| !s.settings("save_hub_service:channel").is_null());
    assert!(s.settings("stratum_user:channel").is_null());
    for id in [
        "channel_field:channel:address",
        "pair_channel:channel",
        "test_channel:channel",
        "save_channel:channel",
    ] {
        assert!(s.settings(id).is_null(), "{id}");
    }
    let before = f.frames(l.ui).len();
    for id in [
        "pair_channel:channel",
        "test_channel:channel",
        "save_channel:channel",
    ] {
        l.control(id);
    }
    inspect(&f, l.ui);
    assert_eq!(
        f.frames(l.ui).len(),
        before,
        "Disabled controls cannot emit actions"
    );
    l.snapshot("settings", "r1_gateway_unconfigured.png");
    l.input("channel_field:channel:credential", "gateway-one");
    l.control("save_hub_service:channel");
    l.until(|s| s.settings("stratum_hub:channel")["height"] == 36);
    assert!(f
        .db("hub.sqlite")
        .query(
            "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
            &[]
        )
        .unwrap()
        .is_empty());
    assert_eq!(f.db("hub.sqlite").query("SELECT json_extract(config_json,'$.credential') AS credential FROM hub_plugin_services WHERE plugin_id='channel'",&[]).unwrap()[0]["credential"],"gateway-one");
    l.snapshot("settings", "r1_gateway_active_collapsed.png");
    l.control("edit_hub_service:channel");
    l.until(|s| !s.settings("channel_field:channel:credential").is_null());
    l.snapshot("settings", "r1_gateway_active_expanded.png");
    l.control("save_hub_service:channel");
    l.until(|s| s.settings("stratum_hub:channel")["height"] == 36);
    assert_eq!(
        l.results
            .iter()
            .rev()
            .find(|(a, _, _)| a["op"] == "save_hub_service")
            .unwrap()
            .0["values"],
        json!({}),
        "Acknowledged secrets must not be resubmitted"
    );
    // Submit both scopes before pumping the daemon, then request a background
    // settings snapshot. Both request acknowledgements must reach the real UI.
    let frame_start = l.frames.len();
    l.control("edit_hub_service:channel");
    l.input("channel_field:channel:credential", "gateway-overlap");
    l.control("save_hub_service:channel");
    l.input("channel_field:channel:address", "personal-room");
    l.control("save_channel:channel");
    let mut saves = Vec::new();
    wait(|| {
        saves = f
            .frames(l.ui)
            .into_iter()
            .filter(|a| {
                (a["op"] == "save_hub_service"
                    && a["values"] == json!({"credential":"gateway-overlap"}))
                    || (a["op"] == "channel_save" && a["values"]["address"] == "personal-room")
            })
            .collect();
        saves.len() == 2
    });
    assert_ne!(saves[0]["request_id"], saves[1]["request_id"]);
    l.control("open_settings");
    wait(|| {
        l.pump();
        let frames = &l.frames[frame_start..];
        frames.iter().enumerate().any(|(index, frame)| {
            frame["op"] == "settings"
                && frame["hub"] == "local"
                && frame["request_id"].is_null()
                && frame["source_op"].is_null()
                && saves.iter().all(|save| {
                    frames[..index].iter().any(|ack| {
                        ack["op"] == "settings" && ack["request_id"] == save["request_id"]
                    })
                })
        })
    });
    let s = inspect(&f, l.ui);
    assert_eq!(
        s.settings("toggle_channel:channel")["text"],
        "Pause Messenger"
    );
    assert_eq!(s.settings("stratum_hub:channel")["height"], 36);
    l.control("toggle_channel:channel");
    l.until(|s| s.settings("toggle_channel:channel")["text"] == "Resume Messenger");
    let paused = f
        .db("hub.sqlite")
        .query(
            "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
            &[],
        )
        .unwrap();
    l.control("edit_hub_service:channel");
    l.until(|s| !s.settings("channel_field:channel:credential").is_null());
    l.input("channel_field:channel:credential", "gateway-two");
    l.control("save_hub_service:channel");
    l.until(|s| s.settings("stratum_hub:channel")["height"] == 36);
    assert_eq!(
        f.db("hub.sqlite")
            .query(
                "SELECT * FROM hub_human_channels WHERE plugin_id='channel'",
                &[]
            )
            .unwrap(),
        paused,
        "Gateway rotation cannot change/resume the personal binding"
    );
    l.control("edit_user_channel:channel");
    l.control("pair_channel:channel");
    l.until(|s| !s.settings("pairing_instruction:channel").is_null());
    l.snapshot("settings", "r1_pairing_real_code.png");
    l.control("test_channel:channel");
    l.until(|s| s.settings("settings_channel_detail:channel")["text"] == "Fixture test succeeded");

    l.control("settings_tab:hubs");
    l.control("open_add_hub");
    l.control("submit_add_hub");
    l.until(|s| {
        s.settings("add_hub_error")["text"] == "Hub URL must start with http:// or https://."
    });
    l.snapshot("settings", "r3_add_hub_validation.png");
    l.input("settings_hub_name", "Office Mac");
    l.input("settings_hub_url", "http://");
    l.control("submit_add_hub");
    l.until(|s| {
        s.settings("add_hub_error")["text"]
            == "Enter a Hub URL using HTTP or HTTPS, without credentials, query, or fragment."
    });
    l.snapshot("settings", "r3_add_hub_daemon_error.png");
    for (name, url, canonical) in [
        (
            "Office Mac",
            format!("http://127.0.0.1:{}/", port()).as_str(),
            "Office-Mac",
        ),
        ("", "  https://studio.example.com/  ", "studio"),
    ] {
        if name.is_empty() {
            l.control("open_add_hub");
        }
        l.input("settings_hub_name", name);
        l.input("settings_hub_url", url);
        l.control("submit_add_hub");
        l.until(|s| {
            s.settings("add_hub_form").is_null()
                && !s.settings(&format!("settings_hub:{canonical}")).is_null()
        });
        let (action, status, _) = l
            .results
            .iter()
            .rev()
            .find(|(action, _, _)| action["op"] == "setup_save_hub" && action["name"] == canonical)
            .unwrap();
        assert_eq!(*status, 200);
        assert!(action["request_id"].is_string());
        assert!(l.frames.iter().any(|frame| frame["op"] == "settings"
            && frame["ok"] == true
            && frame["source_op"] == action["op"]
            && frame["request_id"] == action["request_id"]));
        l.control(&format!("remove_hub:{canonical}"));
        l.until(|s| s.settings(&format!("settings_hub:{canonical}")).is_null());
    }
    l.control("open_add_hub");
    l.input("settings_hub_name", "scratch");
    l.input("settings_hub_url", &format!("http://127.0.0.1:{}", port()));
    l.control("submit_add_hub");
    l.until(|s| {
        s.settings("add_hub_form").is_null() && !s.settings("settings_hub:scratch").is_null()
    });
    // The same form remains open through validation and daemon errors, and closes
    // only on a real settings/state acknowledgement of the saved Hub.
    l.control("toggle_host_hub");
    l.until(|s| s.settings("toggle_host_hub")["text"] == "Stop Sharing");
    l.input("host_hub_port", &port().to_string());
    l.control("apply_host_hub_port");
    l.until(|s| s.settings("apply_host_hub_port")["text"] == "✓ Saved");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = occupied.local_addr().unwrap().port().to_string();
    l.input("host_hub_port", &busy);
    l.control("apply_host_hub_port");
    let failed_port = l.until(|s| !s.settings("host_hub_error").is_null());
    assert!(failed_port.settings("settings_error_banner").is_null());
    assert_eq!(
        failed_port.settings("host_hub_error")["text"],
        "Cannot start sharing: Address already in use"
    );
    assert!(failed_port.settings("host_hub_error")["tooltip"]
        .as_str()
        .unwrap()
        .contains("Cannot start sharing:"));
    l.snapshot("settings", "r3_host_hub_error.png");
    l.control("settings_tab:apps");
    l.control("settings_tab:hubs");
    let s = l.until(|s| s.settings("host_hub_port")["text"] == busy);
    assert_eq!(s.settings("host_hub_port")["text"], busy);
    l.control("dismiss_settings_error");
    l.until(|s| s.settings("settings_error_banner").is_null());
    l.control("paste_hub_invite");
    l.until(|s| {
        s.settings("settings_error_label")["text"]
            .as_str()
            .is_some_and(|t| t.contains("Clipboard"))
    });
    l.control("dismiss_settings_error");
    // JSON invites use the daemon parser. CLI/URL formats are also exercised by
    // the invite round-trip and parser tests, without connecting to real services.
    f.send(l.ui,json!({"op":"test_clipboard","text":json!({"name":"pasted","url":format!("http://127.0.0.1:{}",port())}).to_string()}));
    l.control("paste_hub_invite");
    let joined = l.until(|s| !s.settings("settings_hub:pasted").is_null());
    assert_eq!(joined.settings("paste_hub_invite")["text"], "✓ Joined");
    l.control("remove_hub:pasted");
    l.until(|s| s.settings("settings_hub:pasted").is_null());
    l.control("remove_hub:scratch");
    l.until(|s| s.settings("settings_hub:scratch").is_null());
    l.control("close_settings");
    l.until(|s| s["settings_open"] == false);

    // A real plugin discovery response travels through SessionActivity, snapshot,
    // and the UI, including Listen and /reload rows in the unified scroll document.
    let options = f.state.join("plugins/fixture/options.json");
    let sessions:Vec<Value>=(0..5).map(|i|json!({"harness":"fixture","session_id":format!("reload-{i}"),"title":"Work session","cwd":"/tmp","endpoint_pid":std::process::id(),"needs_reload":i%2==0,"reload_hint":"Run /reload in Fixture to enable 1-click listen"})).collect();
    fs::write(&options,json!({"session_id":"session-1","status":"alive","outcome":"delivered_unconfirmed","sessions":sessions}).to_string()).unwrap();
    f.action_ok(json!({"op":"setup_apply","harness":"fixture"}));
    f.send(l.ui, json!({"op":"test_menu_open"}));
    l.until(|s| s.menu("toggle_available").is_object());
    l.control("toggle_available");
    l.until(|s| !s.menu("reload:fixture:reload-0").is_null());
    l.snapshot("menu", "r4_needs_reload_candidates.png");
    l.control("reload:fixture:reload-0");
    l.until(|s| s["clipboard"] == "/reload");
    l.until(|s| s.menu("listen:fixture:reload-1")["enabled"] == true);
    l.control("listen:fixture:reload-1");
    l.until(|s| !s.menu("menu_error_row").is_null());
    l.snapshot("menu", "r3_menu_error.png");
    l.control("dismiss_menu_error");
    l.until(|s| s.menu("menu_error_row").is_null());

    l.control("open_directory");
    let s = l.until(|s| {
        s["directory_open"] == true && s["directory_matched_count"].as_u64().is_some_and(|n| n > 0)
    });
    l.snapshot("directory", "r5_real_directory.png");
    let msg = nodes(&s["directory_items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("msg_dir:"))
                .map(str::to_string)
        })
        .unwrap();
    l.control(&msg);
    l.until(|s| s["composer_open"] == true);
    f.send(
        l.ui,
        json!({"op":"test_composer_input","value":"Closed-loop message"}),
    );
    l.control("composer_send");
    let s = l.until(|s| {
        s.directory("composer_error")["text"]
            .as_str()
            .is_some_and(|t| t.contains("Cannot send to your own session"))
    });
    assert_eq!(s.directory("composer_input")["text"], "Closed-loop message");
    l.snapshot("directory", "r3_real_composer_error.png");
    l.control("composer_cancel");
    f.action_ok(json!({"op":"listen_session","harness":"fixture","session_id":"session-1"}));
    f.send(l.ui, json!({"op":"test_directory_query","query":""}));
    let target = f
        .rows("SELECT public_id FROM local_listeners WHERE harness='fixture' AND status='ACTIVE'")
        [0]["public_id"]
        .as_str()
        .unwrap()
        .to_string();
    let message = format!("msg_dir:{target}");
    l.until(|s| !s.directory(&message).is_null());
    l.control(&message);
    l.until(|s| s["composer_open"] == true);
    f.send(
        l.ui,
        json!({"op":"test_composer_input","value":"Closed-loop delivery"}),
    );
    l.control("composer_send");
    l.until(|s| s["composer_open"] == false);
    l.snapshot("directory", "r5_real_composer_acknowledged.png");
    for op in [
        "state",
        "settings",
        "directory",
        "pair_code",
        "channel_test_result",
        "composer_result",
        "error",
    ] {
        assert!(
            l.frames.iter().any(|frame| frame["op"] == op),
            "Missing real daemon frame: {op}"
        );
    }
    // Both Apply submissions and the sharing toggle own correlated receipts.
    assert_eq!(
        l.results
            .iter()
            .filter(
                |(action, _, _)| action["op"] == "set_host_hub" && action["request_id"].is_string()
            )
            .count(),
        3
    );
    // Composer failures settle on their result frame; host errors retain the action tag.
    for (action, status, _) in &l.results {
        let result_op = match action["op"].as_str() {
            Some("send_signal") => "composer_result",
            Some("set_host_hub") if action["request_id"].is_string() => "settings",
            _ => continue,
        };
        assert!(!action["request_id"].as_str().unwrap().is_empty());
        assert!(l
            .frames
            .iter()
            .any(|frame| frame["op"] == result_op && frame["request_id"] == action["request_id"]));
        if *status >= 400 {
            let matching = l
                .frames
                .iter()
                .filter(|frame| frame["request_id"] == action["request_id"]);
            if result_op == "composer_result" {
                let receipts: Vec<_> = matching.collect();
                assert_eq!(
                    receipts.len(),
                    1,
                    "Composer failure owns one result: {receipts:?}"
                );
                assert_eq!(receipts[0]["op"], "composer_result");
                assert_eq!(receipts[0]["ok"], false);
                assert!(receipts[0]["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty()));
                continue;
            }
            assert!(
                matching
                    .clone()
                    .any(|frame| (frame["ok"] == false || frame["op"] == "error")
                        && frame["source_op"] == action["op"]
                        && (frame["error"].is_string()
                            || frame["settings"]["host_hub"]["error"].is_string())),
                "Missing failure receipt for {action}: {:?}",
                matching.collect::<Vec<_>>()
            );
        }
    }
    for (name, op) in [("Office-Mac", "error"), ("scratch", "settings")] {
        let (action, _, _) = l
            .results
            .iter()
            .find(|(action, _, _)| action["op"] == "setup_save_hub" && action["name"] == name)
            .unwrap();
        assert!(!action["request_id"].as_str().unwrap().is_empty());
        assert!(l.frames.iter().any(|frame| frame["op"] == op
            && frame["source_op"] == action["op"]
            && frame["request_id"] == action["request_id"]));
    }
}

#[test]
fn u3_u11_visual_states_strata_navigation_labels_and_retina_snapshots() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut cfg = settings();
    cfg["settings_hub"] = json!("local");
    cfg["hubs"][1]["url"] = json!("https://cito.example.com");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["channels"][0]["status"] = json!("not_configured");
    cfg["channels"][0]["hub_ready"] = json!(false);
    cfg["channels"][0]["fields"][0]["configured"] = json!(false);
    cfg["channels"][0]["fields"][0]["value"] = json!("");
    cfg["channels"][0]["setup_guide"] = json!([
        "1. Create a gateway credential with your provider.",
        "2. Save it once for everyone on this Hub."
    ]);
    cfg["hubs"][0]["name"] = json!("local");
    let mut frame = state();
    frame["hubs"][0]["name"] = json!("local");
    frame["listeners"] = json!([]);
    for hub in cfg["hubs"].as_array_mut().unwrap() {
        hub["human_paused"] = json!(true);
        hub["human_state_status"] = json!("current");
        hub["human_control_revision"] = json!(7);
    }
    frame["settings"] = cfg.clone();
    for hub in frame["hubs"].as_array_mut().unwrap() {
        hub["human_paused"] = json!(true);
        hub["human_state_status"] = json!("current");
        hub["human_control_revision"] = json!(7);
        hub["human_availability"] = json!("UNLISTENED");
    }
    frame["available"] =
        json!([{"harness":"fixture","session_id":"s1","title":"Work session","cwd":"/tmp"}]);
    f.send(p, frame.clone());
    control(&f, p, "inbox_paused_notice");
    let s = inspect(&f, p);
    assert!(s.settings("inbox_control_bar")["height"].as_f64().unwrap() >= 48.0);
    assert_eq!(s.settings("toggle_inbox")["text"], "Resume Inbox");
    assert_eq!(s.settings("toggle_inbox")["enabled"], true);
    if std::env::var_os("CITO_TEST_SNAPSHOT_DIR").is_some() {
        render_png(&f, p, "menu", "menu_human_paused.png");
    } else {
        capture_png(
            &f,
            p,
            "menu",
            &f.home.join("menu_human_paused.png"),
            None,
            None,
        );
    }
    let rendered = inspect(&f, p);
    for item in nodes(&s["items"]) {
        if let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) {
            let restored = rendered.menu(id);
            for coordinate in ["x", "y", "width", "height"] {
                assert_eq!(
                    restored[coordinate], item[coordinate],
                    "Rendering must restore {id}.{coordinate}"
                );
            }
        }
    }
    for (status, expected) in [
        ("auth_required", "Sign in required"),
        ("connecting", "Connecting…"),
        ("unreachable", "Hub unavailable"),
        ("error", "Hub unavailable"),
    ] {
        let mut unavailable = frame.clone();
        unavailable["hubs"][0]["status"] = json!(status);
        unavailable["settings"]["hubs"][0]["status"] = json!(status);
        f.send(p, unavailable);
        control(&f, p, "target_hub:local");
        let snapshot = inspect(&f, p);
        assert_eq!(snapshot.menu("header_subtitle")["text"], expected);
        assert_eq!(snapshot.settings("toggle_inbox")["enabled"], false);
        assert_eq!(snapshot.menu("inbox_paused_notice")["height"], 24);
        render_png(&f, p, "menu", &format!("inbox_{status}.png"));
    }
    // Resume uses shared policy CAS and never sends a replacement description.
    for about in [
        Value::Null,
        json!("Human"),
        json!("You (@alice)"),
        json!("Release approvals"),
        json!("café notes"),
    ] {
        let mut paused = frame.clone();
        paused["hubs"][0]["human_about"] = about;
        f.send(p, paused.clone());
        assert_eq!(inspect(&f, p).menu("inbox_paused_notice")["height"], 24);
        control(&f, p, "toggle_inbox");
        let resume = latest(&f, p, "human_inbox_update");
        assert_eq!(resume["hub"], "local");
        assert_eq!(resume["paused"], false);
        assert_eq!(resume["expected_control_revision"], 7);
        assert!(resume.get("about").is_none());
        paused["hubs"][0]["human_paused"] = json!(false);
        paused["hubs"][0]["human_control_revision"] = json!(8);
        paused["hubs"][0]["human_availability"] = json!("ACTIVE");
        paused["settings"]["hubs"][0]["human_paused"] = json!(false);
        paused["settings"]["hubs"][0]["human_control_revision"] = json!(8);
        paused["source_op"] = json!("human_inbox_update");
        paused["request_id"] = resume["request_id"].clone();
        f.send(p, paused);
        assert!(
            inspect(&f, p).settings("inbox_control_bar")["height"]
                .as_f64()
                .unwrap()
                >= 48.0
        );
    }
    f.send(p, frame.clone());
    control(&f, p, "open_settings");
    control(&f, p, "channel_hub_selector:local");
    control(&f, p, "settings_tab:apps");
    let s = inspect(&f, p);
    assert!(s.settings("onboarding_banner").is_null());
    assert!(s.settings("apps_next_step_callout").is_null());
    let tier = s.settings("settings_app_tier:cursor");
    assert_eq!(tier["text"], "Experimental");
    assert!(tier["width"].as_f64().unwrap() < 88.0);
    render_png(&f, p, "settings", "settings_apps.png");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        assert!(s.settings("onboarding_banner").is_null());
        assert_eq!(s.settings(&format!("settings_{tab}_tab"))["height"], 368);
    }
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert!(s.settings("onboarding_banner").is_null());
    assert_eq!(
        s.settings("settings_channel_status:channel")["text"],
        "Not Set Up"
    );
    let hub = s.settings("stratum_hub:channel");
    assert!(s.settings("stratum_user:channel").is_null());
    assert!(nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:secret"));
    assert!(!nodes(&hub["children"])
        .iter()
        .any(|n| n["id"] == "channel_field:channel:account"));
    assert_eq!(
        s.settings("stratum_hub_title:channel")["text"],
        "1. Shared Hub Service"
    );
    assert!(s.settings("stratum_user_title:channel").is_null());
    render_png(&f, p, "settings", "settings_channels_unconfigured.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":0}));
    render_png(&f, p, "settings", "settings_channels_unconfigured_node.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    let before = f.frames(p).len();
    control(&f, p, "skip_channel:channel");
    assert_eq!(inspect(&f, p)["settings_tab"], "channels");
    assert_eq!(
        f.frames(p).len(),
        before,
        "Hidden prerequisite actions cannot mutate Hub state"
    );
    control(&f, p, "settings_tab:apps");
    assert!(inspect(&f, p).settings("apps_next_step_callout").is_null());
    cfg["channels"][0]["status"] = json!("pairing_pending");
    cfg["channels"][0]["hub_ready"] = json!(true);
    cfg["channels"][0]["fields"][0]["configured"] = json!(true);
    cfg["channels"][0]["pairing_instruction"] =
        json!("Send /start cito-abc123def4 to your account provider.");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-abc123def4");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:channels");
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("stratum_hub_status:channel")["text"],
        "Service Ready"
    );
    assert!(!s.settings("clear_channel_service:channel").is_null());
    assert!(nodes(&s.settings("stratum_user:channel")["children"])
        .iter()
        .any(|n| n["id"] == "pairing_instruction:channel"));
    let copy = s.settings("copy_pair_command:channel");
    assert!(
        copy["label_measured_width"].as_f64().unwrap() <= copy["label_width"].as_f64().unwrap()
    );
    render_png(&f, p, "settings", "settings_channels_pairing.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":0}));
    render_png(&f, p, "settings", "settings_channels_pairing_node.png");
    f.send(p, json!({"op":"test_settings_scroll","offset":100000}));
    control(&f, p, "copy_pair_command:channel");
    assert_eq!(inspect(&f, p)["clipboard"], "/start cito-abc123def4");
    cfg["channels"][0]["configured"] = json!(true);
    cfg["channels"][0]["status"] = json!("ready");
    cfg["channels"][0]["pairing_instruction"] = json!("");
    cfg["channels"][0]["pairing_command"] = json!("");
    cfg["channels"][0]["fields"][1]["configured"] = json!(true);
    cfg["channels"][0]["fields"][1]["value"] = json!("account-42");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let paired = inspect(&f, p);
    assert_eq!(paired.settings("stratum_user:channel")["height"], 60);
    assert!(paired.settings("channel_field:channel:account").is_null());
    assert_eq!(
        paired.settings("channel_summary:channel")["text"],
        "Paired · account-42"
    );
    control(&f, p, "edit_user_channel:channel");
    assert!(!inspect(&f, p)
        .settings("channel_field:channel:account")
        .is_null());
    control(&f, p, "save_channel:channel");
    wait(|| f.frames_for(p, "channel_save").next().is_some());
    let save = f.frames_for(p, "channel_save").next_back().unwrap();
    assert!(!inspect(&f, p).settings("save_channel:channel").is_null());
    f.send(p,json!({"op":"settings","ok":true,"source_op":"channel_save","channel_id":"channel","hub":save["hub"],"request_id":save["request_id"],"settings":cfg}));
    let saved = inspect(&f, p);
    assert_eq!(
        saved.settings("stratum_hub_title:channel")["text"],
        "Hub Service"
    );
    assert_eq!(saved.settings("stratum_hub_status:channel")["hidden"], true);
    for id in [
        "edit_hub_service",
        "clear_channel_service",
        "test_channel",
        "toggle_channel",
        "edit_user_channel",
        "unpair_channel",
    ] {
        let button = saved.settings(&format!("{id}:channel"));
        assert!(
            button["label_width"].as_f64().unwrap()
                >= button["label_measured_width"].as_f64().unwrap(),
            "{id}: {button}"
        );
    }
    assert_eq!(
        saved.settings("clear_channel_service:channel")["foreground"],
        saved.settings("edit_hub_service:channel")["foreground"]
    );
    assert_eq!(
        saved.settings("channel_summary:channel")["text"],
        "Paired · account-42 · ✓ Saved"
    );
    assert!(saved.settings("save_channel:channel").is_null());
    let reserved_toggle = saved.settings("toggle_channel:channel");
    for enabled in [false, true] {
        cfg["channels"][0]["enabled"] = json!(enabled);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let toggled = inspect(&f, p).settings("toggle_channel:channel");
        assert_eq!(
            toggled["text"],
            if enabled {
                "Pause Human Channel"
            } else {
                "Resume Human Channel"
            }
        );
        for dimension in ["x", "width"] {
            assert_eq!(
                toggled[dimension], reserved_toggle[dimension],
                "{dimension}"
            );
        }
        control(&f, p, "toggle_channel:channel");
        let pending = inspect(&f, p).settings("toggle_channel:channel");
        assert_eq!(
            pending["text"],
            if enabled { "Pausing…" } else { "Resuming…" }
        );
        assert_eq!(pending["width"], toggled["width"]);
        confirm_channel_action(&f, p, "channel_toggle");
    }
    render_png(&f, p, "settings", "settings_channels_paired.png");
    // An active pairing command always expands even an already configured account.
    cfg["channels"][0]["pairing_instruction"] = json!("Send /start cito-abc123def4 to @bot");
    cfg["channels"][0]["pairing_command"] = json!("/start cito-abc123def4");
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert!(!inspect(&f, p)
        .settings("pairing_instruction:channel")
        .is_null());
    // Non-admins see their personal account without a shared-service wizard.
    cfg["channels"][0]["hub_provided"] = json!(true);
    cfg["channels"][0]["can_configure_hub"] = json!(false);
    cfg["channels"][0]["can_clear_hub"] = json!(false);
    cfg["channels"][0]["fields"][0]["hidden"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    assert!(s.settings("channel_field:channel:secret").is_null());
    assert!(s.settings("clear_channel_service:channel").is_null());
    assert!(s.settings("stratum_hub:channel").is_null());
    render_png(&f, p, "settings", "r1_gateway_non_admin.png");
    cfg["channel_inventory_error"] = json!("Unable to load channel settings.");
    cfg["channels"] = Value::Null;
    f.send(p, json!({"op":"settings","settings":cfg}));
    let unavailable = inspect(&f, p);
    assert!(unavailable.settings("stratum_hub:channel").is_null());
    assert_eq!(
        unavailable.settings("channel_inventory_error")["text"],
        "Unable to load channel settings."
    );
    assert!(
        !unavailable.settings("settings_channel:channel").is_null(),
        "retain the last inventory"
    );
    cfg["channel_inventory_error"] = Value::Null;
    cfg["host_hub"]["auth_mode"] = json!("token");
    cfg["host_hub"]["token"] = json!("12345678abcdef0123456789");
    cfg["host_hub"]["enabled"] = json!(true);
    cfg["host_hub"]["endpoint_url"] = json!("http://mac-studio.tailnet.ts.net:4291");
    cfg["host_hub"]["tailscale_url"] = cfg["host_hub"]["endpoint_url"].clone();
    cfg["host_hub"]["tailscale_invite_command"] =
        json!("cito setup --hub mac-studio --url http://mac-studio.tailnet.ts.net:4291");
    cfg["host_hub"]["invite_command"] = cfg["host_hub"]["tailscale_invite_command"].clone();
    cfg["host_hub"]["lan_url"] = json!("http://192.168.1.10:4291");
    cfg["host_hub"]["lan_invite_command"] =
        json!("cito setup --hub mac-studio --url http://192.168.1.10:4291");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["hubs"][1]["status"] = json!("auth_required");
    cfg["hubs"][1]["can_login"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "settings_tab:hubs");
    let sharing_toggle = inspect(&f, p).settings("toggle_host_hub");
    for (running, error) in [
        (false, None),
        (false, Some("Address already in use")),
        (
            false,
            Some(
                "The port is already in use.\nStop the other service or choose a different port.\nThen apply the new port to resume sharing.",
            ),
        ),
        (false, Some("")),
        (true, Some("")),
        (true, Some("Hub listener failed")),
        (true, None),
    ] {
        let failure = error.filter(|message| !message.is_empty());
        cfg["host_hub"]["running"] = json!(running);
        cfg["host_hub"]["error"] = json!(error);
        f.send(p, json!({"op":"settings","settings":cfg}));
        let status = inspect(&f, p);
        let toggle = status.settings("toggle_host_hub");
        assert_eq!(toggle["text"], "Stop Sharing");
        let dot = status.settings("host_hub_status");
        assert_eq!(dot["width"], 6);
        assert_eq!(
            dot["tooltip"],
            failure.unwrap_or(if running {
                "Sharing is running"
            } else {
                "Sharing is starting"
            })
        );
        assert_eq!(toggle["width"], sharing_toggle["width"]);
        assert_eq!(toggle["x"], sharing_toggle["x"]);
        let prominent = status.settings("settings_tab:hubs")["background"].clone();
        let invite = status.settings("copy_hub_invite");
        if running && failure.is_none() {
            assert_ne!(toggle["background"], prominent);
            assert_eq!(invite["background"], prominent);
        } else {
            assert_ne!(invite["background"], prominent);
            if failure.is_none() {
                assert_ne!(toggle["background"], prominent);
            }
        }
        let warning = status.settings("host_hub_error");
        if !running && error.is_none() {
            render_png(&f, p, "settings", "r3_host_hub_starting.png");
        }
        if error == Some("Address already in use") {
            render_png(&f, p, "settings", "r3_host_hub_failed.png");
        }
        if let Some(failure) = failure {
            assert_eq!(warning["text"], failure);
            assert_eq!(warning["wraps"], true);
            if failure.contains('\n') {
                assert!(warning["height"].as_f64().unwrap() > 32.0);
            }
        } else {
            assert!(warning.is_null());
            let title = status.settings("host_hub_title");
            assert_eq!(
                title["text"].as_str().unwrap().contains("Starting…"),
                !running
            );
        }
        if !running || failure.is_some() {
            assert!(
                status.settings("copy_hub_invite")["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("unavailable")
            );
        }
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"4292"}),
    );
    cfg["host_hub"]["enabled"] = json!(false);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let off = inspect(&f, p);
    let toggle = off.settings("toggle_host_hub");
    assert_eq!(toggle["text"], "Start Sharing");
    for dimension in ["x", "width"] {
        assert_eq!(toggle[dimension], sharing_toggle[dimension]);
    }
    assert_eq!(
        off.settings("host_hub_error")["text"],
        "Choose Start Sharing to accept connections"
    );
    let off_invite = off.settings("copy_hub_invite");
    assert_eq!(
        off_invite["tooltip"],
        "Enable sharing and apply the port before inviting other Macs"
    );
    assert_eq!(off_invite["background"], toggle["background"]);
    render_png(&f, p, "settings", "r3_host_hub_off_with_draft.png");
    cfg["host_hub"]["error"] = json!("Port already in use");
    f.send(p, json!({"op":"settings","settings":cfg}));
    let off_error = inspect(&f, p);
    assert_eq!(
        off_error.settings("toggle_host_hub")["text"],
        "Start Sharing"
    );
    assert_eq!(
        off_error.settings("host_hub_error")["text"],
        "Port already in use"
    );
    cfg["host_hub"]["error"] = Value::Null;
    cfg["host_hub"]["enabled"] = json!(true);
    f.send(p, json!({"op":"settings","settings":cfg}));
    let s = inspect(&f, p);
    for (hub, trailing) in [("local", "toggle_hub:local"), ("work", "remove_hub:work")] {
        let card = s.settings(&format!("settings_hub:{hub}"));
        let button = s.settings(trailing);
        let edit = s.settings(&format!("edit_hub:{hub}"));
        assert_eq!(
            button["x"].as_f64().unwrap() + button["width"].as_f64().unwrap(),
            card["width"].as_f64().unwrap() - 12.0
        );
        let toggle = s.settings(&format!("toggle_hub:{hub}"));
        assert_eq!(
            edit["x"].as_f64().unwrap() + edit["width"].as_f64().unwrap() + 6.0,
            toggle["x"].as_f64().unwrap()
        );
        if hub == "work" {
            assert_eq!(
                toggle["x"].as_f64().unwrap() + toggle["width"].as_f64().unwrap() + 6.0,
                button["x"].as_f64().unwrap()
            );
        }
    }
    let host = s.settings("host_hub_card");
    let toggle = s.settings("toggle_host_hub");
    assert_eq!(
        host["height"].as_f64().unwrap()
            - toggle["y"].as_f64().unwrap()
            - toggle["height"].as_f64().unwrap(),
        10.0
    );
    let login = s.settings("login_hub:work");
    let edit = s.settings("edit_hub:work");
    assert_eq!(
        login["x"].as_f64().unwrap() + login["width"].as_f64().unwrap() + 6.0,
        edit["x"].as_f64().unwrap()
    );
    let token = s.settings("copy_host_hub_token");
    assert!(!token.is_null());
    let apply = s.settings("apply_host_hub_port");
    let access = s.settings("toggle_host_hub_auth");
    assert!(
        apply["x"].as_f64().unwrap() + apply["width"].as_f64().unwrap()
            < token["x"].as_f64().unwrap()
    );
    assert!(
        token["x"].as_f64().unwrap() + token["width"].as_f64().unwrap()
            < access["x"].as_f64().unwrap()
    );
    render_png(&f, p, "settings", "settings_hubs_token.png");
    control(&f, p, "copy_hub_invite");
    assert_eq!(
        inspect(&f, p)["clipboard"],
        cfg["host_hub"]["invite_command"]
    );
    for address in ["lan", "tailscale"] {
        control(&f, p, &format!("host_hub_addr:{address}"));
        assert_eq!(
            inspect(&f, p).settings("host_hub_endpoint")["text"],
            cfg["host_hub"][format!("{address}_url")]
        );
        control(&f, p, "copy_hub_invite");
        assert_eq!(
            inspect(&f, p)["clipboard"],
            cfg["host_hub"][format!("{address}_invite_command")]
        );
    }
    // A dirty port keeps the sharing editor open while sharing is off, so
    // both access labels participate in the same paired-width assertion.
    f.send(p,json!({"op":"test_settings_input","id":"host_hub_port","value":cfg["host_hub"]["port"].as_u64().unwrap().to_string()}));
    let mut reserved = None;
    for enabled in [false, true] {
        let mut toggled = cfg.clone();
        toggled["host_hub"]["enabled"] = json!(enabled);
        toggled["hubs"][0]["enabled"] = json!(enabled);
        toggled["host_hub"]["auth_mode"] = json!(if enabled { "token" } else { "none" });
        f.send(p, json!({"op":"settings","settings":toggled}));
        let snapshot = inspect(&f, p);
        let frames: Vec<_> = [
            "toggle_host_hub",
            "toggle_host_hub_auth",
            "toggle_hub:local",
            "edit_hub:local",
        ]
        .into_iter()
        .map(|id| {
            let view = snapshot.settings(id);
            assert!(!view.is_null(), "{id}");
            (view["x"].clone(), view["width"].clone())
        })
        .collect();
        if let Some(previous) = &reserved {
            assert_eq!(&frames, previous);
        } else {
            reserved = Some(frames);
        }
    }
    f.send(p, json!({"op":"settings","settings":cfg}));
    let snapshot = inspect(&f, p);
    for id in ["settings_tab_track", "host_hub_addr_track"] {
        assert!((snapshot.settings(id)["background"][3].as_f64().unwrap() - 0.06).abs() < 0.001);
    }
    for invalid in ["0", "65536", "word", ""] {
        let before = f.frames(p).len();
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":invalid}),
        );
        control(&f, p, "apply_host_hub_port");
        let s = inspect(&f, p);
        assert_eq!(s.settings("apply_host_hub_port")["text"], "Apply");
        let error = s.settings("host_hub_error");
        assert_eq!(error["text"], "Port must be an integer from 1 to 65535.");
        assert_eq!(error["hidden"], false);
        assert_eq!(f.frames(p).len(), before);
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5301"}),
    );
    control(&f, p, "settings_tab:hubs");
    let error = inspect(&f, p).settings("host_hub_error");
    assert!(
        error.is_null() || error["hidden"] == true,
        "Editing the port clears visible local validation feedback: {error}"
    );
    // Only a command acknowledgement may settle Apply, including an unchanged save.
    frame["settings"] = cfg.clone();
    f.send(p, frame.clone());
    for op in ["state", "settings"] {
        let current = cfg["host_hub"]["port"].as_i64().unwrap();
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":current.to_string()}),
        );
        f.send(p, json!({"op":"settings","settings":cfg}));
        let unchanged = inspect(&f, p);
        assert_eq!(unchanged.settings("apply_host_hub_port")["enabled"], false);
        let before = f.frames(p).len();
        control(&f, p, "apply_host_hub_port");
        inspect(&f, p);
        assert_eq!(
            f.frames(p).len(),
            before,
            "Unchanged Apply must not emit a mutation"
        );
        let draft = current + 1;
        f.send(
            p,
            json!({"op":"test_settings_input","id":"host_hub_port","value":format!(" {draft} ")}),
        );
        control(&f, p, "apply_host_hub_port");
        wait(|| {
            f.frames(p)[before..]
                .iter()
                .any(|v| v["op"] == "set_host_hub" && v["port"] == draft)
        });
        let submitted = f.frames(p)[before..]
            .iter()
            .find(|v| v["op"] == "set_host_hub")
            .unwrap()
            .clone();
        assert!(!submitted["request_id"].as_str().unwrap().is_empty());
        f.send(
            p,
            if op == "state" {
                frame.clone()
            } else {
                json!({"op":"settings","settings":cfg})
            },
        );
        assert_eq!(
            inspect(&f, p).settings("apply_host_hub_port")["text"],
            "Applying…",
            "An uncorrelated {op} snapshot cannot acknowledge the save"
        );
        cfg["host_hub"]["port"] = json!(draft);
        f.send(
            p,
            json!({"op":"settings","request_id":submitted["request_id"],"settings":cfg}),
        );
        assert_eq!(
            inspect(&f, p).settings("apply_host_hub_port")["text"],
            "✓ Saved"
        );
        frame["settings"] = cfg.clone();
    }
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":" 5301 "}),
    );
    control(&f, p, "apply_host_hub_port");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "set_host_hub" && v["port"] == 5301)
    });
    let submitted = f.frames_for(p, "set_host_hub").next_back().unwrap();
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5302"}),
    );
    let before = f.frames(p).len();
    for id in ["toggle_host_hub_auth", "toggle_host_hub"] {
        control(&f, p, id);
    }
    inspect(&f, p);
    let toggles = f.frames(p)[before..].to_vec();
    assert_eq!(toggles.len(), 2);
    for action in toggles {
        assert_eq!(action["op"], "set_host_hub");
        assert_eq!(action["port"], 5302);
        assert!(
            action["request_id"].is_string() && action["request_id"] != submitted["request_id"],
            "Toggles own distinct receipts without replacing the port save"
        );
    }
    // Neither untagged errors nor refreshes may release Apply's pending identity.
    for source in [json!({}), json!({"source_op":"set_host_hub"})] {
        let mut error = json!({"op":"error","error":"Unrelated host error"});
        error
            .as_object_mut()
            .unwrap()
            .extend(source.as_object().unwrap().clone());
        f.send(p, error);
    }
    cfg["host_hub"]["port"] = json!(5301);
    f.send(p, json!({"op":"settings","settings":cfg}));
    assert_eq!(
        inspect(&f, p).settings("apply_host_hub_port")["text"],
        "Applying…"
    );
    f.send(
        p,
        json!({"op":"settings","request_id":submitted["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("apply_host_hub_port")["text"],
        "Apply",
        "An older correlated save must settle without marking the newer draft saved"
    );
    assert_eq!(s.settings("host_hub_port")["text"], "5302");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"host_hub_port","value":"5303"}),
    );
    control(&f, p, "apply_host_hub_port");
    let failed = latest(&f, p, "set_host_hub");
    cfg["host_hub"]["error"] = json!("Port already in use");
    f.send(
        p,
        json!({"op":"settings","request_id":failed["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(s.settings("apply_host_hub_port")["text"], "Apply");
    assert_eq!(s.settings("host_hub_error")["text"], "Port already in use");
    control(&f, p, "apply_host_hub_port");
    let retry = latest(&f, p, "set_host_hub");
    assert_ne!(retry["request_id"], failed["request_id"]);
    f.send(p, json!({"op":"error","source_op":"set_host_hub","request_id":failed["request_id"],"error":"Late port error"}));
    f.send(
        p,
        json!({"op":"settings","request_id":failed["request_id"],"settings":cfg}),
    );
    assert_eq!(
        inspect(&f, p).settings("apply_host_hub_port")["text"],
        "Applying…"
    );
    cfg["host_hub"]["error"] = Value::Null;
    cfg["host_hub"]["port"] = json!(5303);
    f.send(
        p,
        json!({"op":"settings","request_id":retry["request_id"],"settings":cfg}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        s.settings("apply_host_hub_port")["text"],
        "✓ Saved",
        "Late failure must not settle the retry"
    );
    assert!(!s.to_string().contains("Late port error"));
    assert_eq!(
        s.settings("settings_error_label")["text"],
        "Unrelated host error"
    );
    control(&f, p, "dismiss_settings_error");
    assert!(inspect(&f, p).settings("settings_error_banner").is_null());
    control(&f, p, "edit_hub:local");
    let s = inspect(&f, p);
    assert!(
        s.settings("host_hub_error").is_null(),
        "A cleared daemon error must not linger locally"
    );
    assert_eq!(
        s.settings("settings_hub_user")["placeholder"],
        "Defaults to your Mac username"
    );
    assert_eq!(s.settings("settings_hub_name"), Value::Null);
    render_png(&f, p, "settings", "settings_hubs_edit_local.png");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_add_hub");
    render_png(&f, p, "settings", "settings_hubs_add_remote.png");
    for mode in ["none", "token", "oidc"] {
        control(&f, p, &format!("settings_hub_auth:{mode}"));
        let s = inspect(&f, p);
        let track = s.settings("settings_hub_auth_track");
        // Selection styling is checked by the contrast oracle. This assertion
        // owns packing geometry rather than implementation-specific RGBA values.
        let mut right = 0.0;
        for candidate in ["none", "token", "oidc"] {
            let pill = s.settings(&format!("settings_hub_auth:{candidate}"));
            let x = pill["x"].as_f64().unwrap();
            assert!(x >= right);
            right = x + pill["width"].as_f64().unwrap();
            assert!(right <= track["width"].as_f64().unwrap());
            assert!(pill["y"].as_f64().unwrap() >= 0.0);
            assert!(
                pill["y"].as_f64().unwrap() + pill["height"].as_f64().unwrap()
                    <= track["height"].as_f64().unwrap()
            );
        }
        let labels: Vec<_> = nodes(&s.settings("add_hub_form")["children"])
            .into_iter()
            .filter_map(|n| n["text"].as_str().map(String::from))
            .collect();
        assert!(labels.contains(&"Hub Name (Optional)".to_owned()));
        assert!(labels.contains(&"Hub URL".to_owned()));
        assert!(labels.contains(&"Access".to_owned()));
    }
    control(&f, p, "open_directory");
    for mode in ["directory", "activity"] {
        control(&f, p, &format!("dir_mode:{mode}"));
        inspect(&f, p);
        render_png(&f, p, "directory", &format!("directory_{mode}.png"));
        for appearance in ["light", "dark"] {
            f.send(p, json!({"op":"test_appearance","appearance":appearance}));
            inspect(&f, p);
        }
    }
}

#[test]
fn address_validation_and_activity_titles_use_projected_data() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    for appearance in ["light", "dark"] {
        for human in [false, true] {
            let mut invalid = state();
            invalid["listeners"][0]["hub"] = json!(if human { "personal" } else { "work" });
            invalid["listeners"][0]["harness"] = json!(if human { "human" } else { "fixture" });
            // DTO and chip agree, but the address violates the ontology/routing rule.
            invalid["listeners"][0]["display_address"] = json!("alice/fixture#abcd");
            f.send(p, invalid);
            control(&f, p, if human { "hub:personal" } else { "hub:work" });
            if human {
                control(&f, p, "open_directory");
                let mut person = peer("invalid-human");
                person["harness"] = json!("human");
                // Human address validation lives in the unified Directory, not an agent card.
                inspect_unchecked(&f, p, Some(appearance));
                let request = f
                    .frames(p)
                    .into_iter()
                    .rev()
                    .find(|a| {
                        matches!(a["op"].as_str(), Some("query_directory" | "open_directory"))
                    })
                    .unwrap();
                f.send(p,json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[person]}));
            }
            assert!(
                !inspect_unchecked(&f, p, Some(appearance))["address_mismatches"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let mut invalid = state();
        invalid["activity"] = json!([{"hub":"personal","signal_id":"jargon","listener_id":"target","from_user":"alice","from_host_name":"host","from_title":"Human Node","target_harness":"human","target_title":"Human Node","target_scoped_id":"personal:target","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]);
        f.send(p, invalid);
        control(&f, p, "open_activity");
        control(&f, p, "act_filter:all");
        assert_eq!(
            inspect_as(&f, p, Some(appearance))["jargon_leaks"],
            json!([])
        );
        let mut clean = state();
        clean["activity"] = json!([]);
        f.send(p, clean);
        let mut user_titles = state();
        user_titles["listeners"][0]["title"] = json!("Human Node");
        user_titles["listeners"][0]["about"] = json!("[Passive] build");
        user_titles["activity"] = json!([{"hub":"personal","signal_id":"user-title","listener_id":"target","from_user":"alice","from_host_name":"host","from_title":"[Passive] Worker","target_harness":"fixture","target_title":"Human Node","target_scoped_id":"personal:target","message":"[Passive] build","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}]);
        f.send(p, user_titles);
        control(&f, p, "act_filter:all");
        let s = inspect_as(&f, p, Some(appearance));
        assert!(s.directory("act_title:user-title")["text"]
            .as_str()
            .unwrap()
            .contains("Human Node"));
        for title in ["Human Node", "Human listening"] {
            let mut shared = state();
            shared["activity"] = json!([
                {"hub":"personal","signal_id":"shared","listener_id":"first","from_user":"Session","from_host_name":"host","from_title":"[Passive] Worker","target_harness":"fixture","target_title":"Review","target_scoped_id":"personal:first","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true},
                {"hub":"work","signal_id":"shared","listener_id":"second","from_user":"alice","from_host_name":"host","from_title":"Sender","target_harness":"fixture","target_title":title,"target_scoped_id":"work:second","message":"Message","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}
            ]);
            f.send(p, shared.clone());
            let s = inspect_as(&f, p, Some(appearance));
            assert_eq!(
                nodes(&s["directory_items"])
                    .iter()
                    .filter(|n| n["id"] == "act_title:shared")
                    .count(),
                2
            );
            assert_eq!(
                s["jargon_leaks"],
                json!([]),
                "User title on the second Hub: {title}"
            );
            shared["activity"][1]["target_harness"] = json!("human");
            f.send(p, shared);
            assert_eq!(
                inspect_as(&f, p, Some(appearance))["jargon_leaks"],
                json!([])
            );
        }
    }
}

#[test]
fn v29_canonical_four_surface_retina_states_in_both_appearances() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let capture = |surface: &str, state: &str| {
        let snapshot = inspect(&f, p);
        let strip = match surface {
            "menu" => {
                let subtitle_width = snapshot.menu("header_subtitle")["width"].as_f64().unwrap();
                Some((
                    "items",
                    "header_hub_strip",
                    4.0,
                    (368.0 - (54.0 + subtitle_width).max(138.0)).max(44.0),
                ))
            }
            "directory" => Some(("directory_items", "directory_harness_scroll", 4.0, 387.0)),
            "settings" => Some((
                "settings_items",
                "channel_hub_strip",
                4.0,
                snapshot.settings("settings_channels_tab")["content_width"]
                    .as_f64()
                    .unwrap()
                    - 114.0,
            )),
            _ => None,
        };
        if let Some((tree, id, spacing, allocated)) = strip {
            let view = node(&snapshot, tree, id);
            if !view.is_null() && view["hidden"] == false {
                let pills: Vec<_> = view["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|p| p["hidden"] == false)
                    .collect();
                let intrinsic = pills
                    .iter()
                    .map(|p| p["width"].as_f64().unwrap())
                    .sum::<f64>()
                    + spacing * pills.len().saturating_sub(1) as f64
                    + if pills.is_empty() { 0.0 } else { 4.0 };
                let width = view["width"].as_f64().unwrap();
                assert!(
                    width <= allocated + 0.5,
                    "{id} exceeds its allocation: {view}"
                );
                assert!(
                    (width - intrinsic.min(allocated)).abs() < 0.5,
                    "{id} must hug its pills without an empty tail: {view}"
                );
                if let Some(first) = pills.first() {
                    assert!((first["x"].as_f64().unwrap() - 2.0).abs() < 0.5);
                    let end = pills
                        .iter()
                        .map(|pill| pill["x"].as_f64().unwrap() + pill["width"].as_f64().unwrap())
                        .fold(0.0, f64::max);
                    assert!(
                        (intrinsic - end - 2.0).abs() < 0.5,
                        "{id} must have exactly 2pt trailing padding: {view}"
                    );
                }
                let x = view["x"].as_f64().unwrap();
                match surface {
                    "menu" => assert!((x + width - 368.0).abs() < 0.5),
                    "directory" => {
                        let separator = snapshot.directory("directory_facet_separator");
                        assert!(
                            x > separator["x"].as_f64().unwrap()
                                + separator["width"].as_f64().unwrap()
                        );
                    }
                    "settings" => assert_eq!(x, 98.0),
                    _ => unreachable!(),
                }
            }
        }
        if surface == "directory" {
            for id in ["dir_mode:directory", "dir_mode:activity"] {
                assert_eq!(snapshot.directory(id)["radius"], 7);
            }
        }
        for appearance in ["light", "dark"] {
            render_png_as(
                &f,
                p,
                surface,
                &format!("v29_{state}_{appearance}.png"),
                Some(appearance),
            );
        }
    };
    let mut cfg = settings();
    cfg["apps"][2]["id"] = json!("claude");
    cfg["apps"][2]["display_name"] = json!("Claude Code");
    cfg["apps"][2]["installed"] = json!(true);
    cfg["apps"][0]["display_name"] = json!("Pi Coding Agent");
    cfg["channels"][0]["id"] = json!("telegram");
    cfg["channels"][0]["display_name"] = json!("Telegram");
    cfg["channels"][0]["pairing_manual_disclosure"] = json!(true);
    cfg["channels"][0]["pairing_open_url_label"] = json!("Open in Telegram ↗");
    cfg["channels"][0]["fields"][1]["label"] = json!("Chat ID");
    cfg["channels"][0]["fields"][1]["placeholder"] = json!("Paired via /start or enter Chat ID");
    cfg["host_hub"]["enabled"] = json!(true);
    cfg["host_hub"]["running"] = json!(true);
    cfg["host_hub"]["endpoint_url"] = json!("http://Office-Mac.example.com:4291");
    cfg["host_hub"]["invite_command"] =
        json!("cito setup --hub Office-Mac --url http://Office-Mac.example.com:4291 --default");
    cfg["host_hub"]["tailscale_url"] = cfg["host_hub"]["endpoint_url"].clone();
    cfg["host_hub"]["tailscale_invite_command"] = cfg["host_hub"]["invite_command"].clone();
    cfg["host_hub"]["lan_url"] = json!("http://192.168.1.10:4291");
    cfg["host_hub"]["lan_invite_command"] =
        json!("cito setup --hub lan --url http://192.168.1.10:4291 --default");
    cfg["hubs"][0]["name"] = json!("local");
    cfg["hubs"][1]["url"] = json!("https://hub.example.com");
    cfg["hubs"][1]["auth_mode"] = json!("oidc");
    cfg["hubs"][1]["issuer"] = json!("https://id.example.com");
    cfg["hubs"][1]["client_id"] = json!("cito");
    let mut frame = state();
    frame["hubs"][0]["name"] = json!("local");
    frame["hubs"][0]["auth_mode"] = json!("none");
    frame["hubs"][1]["auth_mode"] = json!("oidc");
    frame["settings"] = cfg;
    frame["harnesses"] = json!([
        {"id":"pi","display_name":"Pi","kind":"agent","sf_symbol":"terminal","tint":"purple","tier":"stable","order":0},
        {"id":"codex","display_name":"Codex","kind":"agent","sf_symbol":"terminal","tint":"blue","tier":"stable","order":1},
        {"id":"claude","display_name":"Claude Code","kind":"agent","sf_symbol":"terminal","tint":"orange","tier":"experimental","order":2}
    ]);
    let mut agent = listener();
    agent["hub"] = json!("local");
    agent["harness"] = json!("pi");
    agent["display_address"] = json!("alice/pi#abcd");
    agent["title"] = json!("Release orchestration");
    agent["host_name"] = json!("Office-Mac.local");
    agent["cwd"] = json!("~/Projects/acme/service");
    let mut human = listener();
    human["hub"] = json!("local");
    human["harness"] = json!("human");
    human["session_id"] = json!("default");
    human["title"] = json!("Human");
    human["user"] = json!("alice");
    human["listener_id"] = json!("c_76e846d115ce642f");
    human["public_id"] = json!("alice-76e846d115ce642f");
    human["scoped_id"] = json!("local:alice-76e846d115ce642f");
    human["display_address"] = json!("@alice");
    human["host_name"] = json!("Office-Mac.local");
    human["channels"] = json!([]);
    human["attachments"] = json!([{"channel_id":"here:human","location":"device","host_id":"here","display_name":"Office-Mac.local","availability":"ACTIVE","is_local":true}]);
    frame["hubs"][0]["attachments"] = human["attachments"].clone();
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_paused"] = json!(false);
    frame["hubs"][0]["human_control_revision"] = json!(1);
    frame["listeners"] = json!([agent, human]);
    frame["available"] = json!([{"harness":"codex","session_id":"review","title":"api-server","cwd":"~/Projects/acme/service","branch":"main"}]);
    frame["unread_count"] = json!(2);
    send_companion(&f, p, frame.clone());
    let s = inspect(&f, p);
    assert_eq!(s["count"], 1);
    assert_eq!(s.menu("plugin_icon:pi")["tint"], json!([]));
    assert_eq!(s.menu("plugin_icon:pi")["x"], 0);
    assert_eq!(s.menu("plugin_icon:pi")["width"], 28);
    let copy = s.menu("copy:personal:repo-abcdefghijkl");
    assert!(nodes(&copy["children"])
        .iter()
        .any(|n| n["x"] == 6 && n["width"] == 12));
    assert!(s.menu("human_inbox_card").is_null());
    for appearance in ["light", "dark"] {
        let snapshot = inspect_as(&f, p, Some(appearance));
        let title = nodes(&snapshot["items"])
            .into_iter()
            .find(|n| n["text"] == "Release orchestration")
            .unwrap()
            .clone();
        assert!(
            title["label_measured_width"].as_f64().unwrap()
                <= title["label_width"].as_f64().unwrap(),
            "Hero title must fit beside the rename pencil in {appearance}: {title}"
        );
        let pauses: Vec<_> = nodes(&snapshot["items"])
            .into_iter()
            .filter(|n| n["id"].as_str().is_some_and(|id| id.starts_with("stop:")))
            .collect();
        assert_eq!(pauses.len(), 1);
        for pause in &pauses {
            assert_eq!(pause["text"], "Stop");
            assert!(pause["symbol"].is_null());
            assert_eq!(pause["border_width"], 1);
            assert_eq!(pause["width"], pauses[0]["width"]);
            assert_eq!(pause["height"], 22);
        }
    }
    capture("menu", "menu_active");
    let mut compact_menu = frame.clone();
    compact_menu["available"] = json!([]);
    compact_menu["hubs"][0]["auto_listen"] = json!(true);
    compact_menu["listeners"][0]["listen_mode"] = json!("manual");
    compact_menu["listeners"][0]["title"] = json!("Release review");
    send_companion(&f, p, compact_menu);
    let site = inspect(&f, p);
    assert_eq!(
        site.menu("toggle_available")["text"],
        "AVAILABLE SESSIONS · 0"
    );
    assert_eq!(site.menu("toggle_available")["symbol"], "chevron.right");
    assert_eq!(site.menu("toggle_auto_listen")["text"], "Auto-Listen: On");
    assert!(site.menu("available_empty_target").is_null());
    capture("menu", "menu_site");
    send_companion(&f, p, frame);

    let mut remote_agent = peer("review");
    remote_agent["hub"] = json!("local");
    remote_agent["scoped_id"] = json!("local:review");
    remote_agent["harness"] = json!("pi");
    remote_agent["display_address"] = json!("bob/pi#8a4f");
    remote_agent["title"] = json!("Review session");
    let mut remote_human = peer("human");
    remote_human["hub"] = json!("local");
    remote_human["scoped_id"] = json!("local:human");
    remote_human["harness"] = json!("human");
    remote_human["title"] = json!("Human");
    remote_human["about"] = json!("Human receiver");
    remote_human["display_address"] = json!("@bob");
    remote_human["repo"] = Value::Null;
    remote_human["branch"] = Value::Null;
    let peers = json!([remote_agent, remote_human]);
    let short = json!({"hub":"local","signal_id":"local:short","listener_id":"c_76e846d115ce642f","from_user":"bob","from_host_name":"laptop","from_harness":"pi","from_title":"Review","from_scoped_id":"local:review","from_display_address":"bob/pi#8a4f","target_harness":"human","target_title":"Human","target_scoped_id":"local:alice-76e846d115ce642f","target_display_address":"@alice","message":"All checks passed on release branch. Ship v2.4 now or hold for docs?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true});
    let mut expanded = short.clone();
    expanded["signal_id"] = json!("local:expanded");
    expanded["message"] =
        json!("First line for review.\nSecond line with details.\nThird line: ready to continue.");
    expanded["held_reason"] = json!("stale_generation");
    let activity = json!([short, expanded]);
    control(&f, p, "open_directory");
    let seeded = inspect(&f, p);
    assert_eq!(
        seeded.directory("dir_meta:local:alice-76e846d115ce642f")["text"],
        "Inbox · Office-Mac"
    );
    assert!(!seeded
        .directory("dir_peer:local:alice-76e846d115ce642f")
        .is_null());
    assert_eq!(seeded.directory("directory_empty_state")["hidden"], true);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":activity,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "directory");
    assert_eq!(s.directory("plugin_icon:pi")["tint"], json!([]));
    assert_eq!(s.directory("plugin_icon:pi")["width"], 28);
    capture("directory", "directory");
    control(&f, p, "msg_dir:local:review");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Ready to review"}),
    );
    assert_eq!(inspect(&f, p)["composer_target"], "local:review");
    capture("directory", "directory_composer");
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:activity");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":activity,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "local:short");
    assert_eq!(s.directory("activity_row:local:short")["height"], 60);
    assert_eq!(s.directory("plugin_icon:pi")["tint"], json!([]));
    assert_eq!(s.directory("plugin_icon:pi")["x"], 0);
    let mut clean = activity.clone();
    clean[1].as_object_mut().unwrap().remove("held_reason");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":peers,"activity":clean,"matched_count":2,"online_count":2,"unread_count":2}),
    );
    control(&f, p, "reply_activity:local:short");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Ship v2.4 now — docs can follow."}),
    );
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert_eq!(reply["composer_target"], "local:review");
    assert_eq!(
        reply.directory("composer_target")["text"],
        "To: bob/pi#8a4f"
    );
    assert_eq!(
        reply.directory("composer_input")["text"],
        "Ship v2.4 now — docs can follow."
    );
    capture("directory", "activity");
    control(&f, p, "composer_cancel");
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "local:expanded");
    let height = s.directory("activity_row:local:expanded")["height"]
        .as_f64()
        .unwrap();
    assert!(height > 60.0 && height < 108.0, "{height}");
    assert_eq!(
        s.directory("act_msg:local:expanded")["height"],
        height - 42.0
    );
    let message = s.directory("act_msg:local:expanded");
    assert_eq!(
        message["y"].as_f64().unwrap() + message["height"].as_f64().unwrap(),
        height - 34.0
    );
    for id in [
        "reply_activity:local:expanded",
        "copy_activity_msg:local:expanded",
        "delete_activity:local:expanded",
    ] {
        assert!(
            s.directory(id)["y"].as_f64().unwrap()
                - (message["y"].as_f64().unwrap() + message["height"].as_f64().unwrap())
                >= 4.0
        );
    }
    capture("directory", "activity_expanded");
    control(&f, p, "reply_activity:local:expanded");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Thanks for the review"}),
    );
    assert_eq!(inspect(&f, p)["composer_reply_signal_id"], "local:expanded");
    capture("directory", "activity_composer");
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);

    control(&f, p, "open_settings");
    for tab in ["apps", "channels", "hubs"] {
        control(&f, p, &format!("settings_tab:{tab}"));
        let s = inspect(&f, p);
        assert_eq!(s["settings_tab"], tab);
        if tab == "apps" {
            for id in ["pi", "codex", "claude"] {
                let icon = s.settings(&format!("plugin_icon:{id}"));
                assert_eq!(icon["tint"], json!([]));
                assert_eq!(icon["x"], 0);
                assert_eq!(icon["y"], 0);
                assert_eq!(icon["width"], 28);
            }
        } else if tab == "channels" {
            for appearance in ["light", "dark"] {
                let snapshot = inspect_as(&f, p, Some(appearance));
                let track = snapshot.settings("channel_hub_strip");
                let viewport = snapshot.settings("settings_channels_tab")["content_width"]
                    .as_f64()
                    .unwrap();
                assert!(track["width"].as_f64().unwrap() <= viewport - 114.0);
                let mut right = 0.0;
                for hub in ["local", "work"] {
                    let pill = snapshot.settings(&format!("channel_hub_selector:{hub}"));
                    let x = pill["x"].as_f64().unwrap();
                    assert!(x >= right);
                    right = x + pill["width"].as_f64().unwrap();
                    assert!(right <= track["width"].as_f64().unwrap());
                    assert!(pill["height"].as_f64().unwrap() <= track["height"].as_f64().unwrap());
                }
            }
            assert_eq!(s.settings("plugin_icon:telegram")["tint"], json!([]));
            assert_eq!(s.settings("plugin_icon:telegram")["width"], 24);
            assert!(
                s.settings("settings_channel:telegram")["height"]
                    .as_f64()
                    .unwrap()
                    <= 254.0
            );
        } else {
            assert_eq!(s.settings("copy_hub_invite")["text"], "Copy Invite Code");
            assert!(s.settings("host_hub_guidance").is_null());
            assert_eq!(s.settings("host_hub_card")["height"], 160);
            assert_eq!(
                s.settings("paste_hub_invite")["text"],
                "Join from Clipboard"
            );
        }
        capture("settings", &format!("settings_{tab}"));
    }
    control(&f, p, "open_add_hub");
    f.send(
        p,
        json!({"op":"test_settings_input","id":"settings_hub_name","value":"studio"}),
    );
    f.send(p, json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://hub.example.com"}));
    let s = inspect(&f, p);
    assert_eq!(s.settings("host_hub_card")["height"], 160);
    assert_eq!(s.settings("add_hub_form")["y"], 8);
    assert_eq!(s.settings("add_hub_form")["height"], 252);
    assert_eq!(
        s.settings("hub_visibility_caption")["text"],
        "Personal Hubs connect your Macs automatically; Team Sign-In (OIDC) Hubs start private to your account (@user) until shared with Team."
    );
    assert!(s.settings("host_hub_guidance").is_null());
    capture("settings", "settings_add_hub");
    let scroll_to_top = || f.send(p, json!({"op":"test_settings_scroll","offset":10000}));
    let coord = |view: &Value, key: &str| view[key].as_f64().unwrap();
    let assert_action_bar_visible = |s: &Value, height| {
        let form = s.settings("add_hub_form");
        let scroll = s.settings("settings_hubs_tab");
        let bottom = coord(&form, "y");
        let clip_bottom = coord(&scroll, "content_y");
        assert_eq!(coord(&form, "height"), height);
        assert!(clip_bottom <= bottom && clip_bottom + coord(&scroll, "height") >= bottom + 44.0);
    };
    scroll_to_top();
    control(&f, p, "settings_hub_auth:oidc");
    let collapsed = inspect(&f, p);
    assert_action_bar_visible(&collapsed, 252.0);
    assert_eq!(
        collapsed.settings("manual_hub_oidc")["text"],
        "▸ Configure OIDC settings manually…"
    );
    assert_eq!(
        collapsed.settings("hub_oidc_discovery_summary")["text"],
        "Sign-in settings (issuer, client ID, port) are auto-discovered from the Hub URL."
    );
    assert!(collapsed.settings("settings_hub_issuer").is_null());
    scroll_to_top();
    control(&f, p, "manual_hub_oidc");
    let expanded = inspect(&f, p);
    assert_action_bar_visible(&expanded, 298.0);
    assert!(expanded.settings("hub_oidc_discovery_summary").is_null());
    let caption = expanded.settings("hub_visibility_caption");
    assert_eq!(caption["y"], 40);
    assert_eq!(caption["height"], 32);
    let form = expanded.settings("add_hub_form");
    let children = form["children"].as_array().unwrap();
    for (i, child) in children.iter().enumerate() {
        let (x, y) = (coord(child, "x"), coord(child, "y"));
        let (right, top) = (x + coord(child, "width"), y + coord(child, "height"));
        assert!(
            x >= 6.0 && y >= 6.0 && right <= 474.0 && top <= 292.0,
            "{child}"
        );
        for other in &children[i + 1..] {
            let (ox, oy) = (coord(other, "x"), coord(other, "y"));
            assert!(
                right <= ox
                    || top <= oy
                    || ox + coord(other, "width") <= x
                    || oy + coord(other, "height") <= y,
                "{child} overlaps {other}"
            );
        }
    }
    capture("settings", "settings_add_hub_manual_oidc");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "edit_hub:local");
    assert!(!inspect(&f, p).settings("settings_hub_user").is_null());
    capture("settings", "settings_edit_local");
    send_companion(
        &f,
        p,
        json!({"op":"error","source_op":"setup_save_hub","error":"INVALID_INPUT: Username is invalid"}),
    );
    let mounted_error = inspect(&f, p);
    assert_eq!(mounted_error["settings_rebuild_deferred"], true);
    assert_eq!(mounted_error.settings("add_hub_form")["height"], 124);
    f.send(p, json!({"op":"test_end_editing"}));
    let local_error = inspect(&f, p);
    let error = local_error.settings("add_hub_error");
    let user = local_error.settings("settings_hub_user");
    assert_eq!(
        local_error.settings("add_hub_form")["height"]
            .as_f64()
            .unwrap(),
        124.0 + error["height"].as_f64().unwrap() + 6.0
    );
    assert!(
        user["y"].as_f64().unwrap()
            >= error["y"].as_f64().unwrap() + error["height"].as_f64().unwrap() + 6.0
    );
    control(&f, p, "cancel_add_hub");
    control(&f, p, "edit_hub:work");
    let remote = inspect(&f, p);
    assert_action_bar_visible(&remote, 252.0);
    assert_eq!(
        remote.settings("hub_oidc_discovery_summary")["text"],
        "OIDC: id.example.com · Auto-discovered"
    );
    assert_eq!(remote.settings("add_hub_form")["y"], 8);
    capture("settings", "settings_edit_remote");
    control(&f, p, "cancel_add_hub");
    control(&f, p, "open_uninstall_confirm");
    let s = inspect(&f, p);
    let sheet = s.settings("uninstall_alert_sheet");
    assert!(sheet["width"].as_f64().unwrap() > 0.0);
    assert_eq!(sheet["on_screen"], false);
    assert_eq!(sheet["attached"], false);
    assert_eq!(s.settings("settings_footer")["hidden"], false);
    capture("settings", "settings_uninstall_modal");
    control(&f, p, "cancel_uninstall_all");
    control(&f, p, "close_settings");
    assert_eq!(inspect(&f, p)["settings_open"], false);
}

#[test]
fn directory_escape_clears_search_before_closing() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_directory");
    f.send(p, json!({"op":"test_directory_query","query":"absent"}));
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"matched_count":0,"online_count":2}),
    );
    f.send(p, json!({"op":"test_focus_field","id":"directory_search"}));
    let filtered = inspect(&f, p);
    assert_eq!(filtered["directory_matched_count"], 0);
    assert_eq!(filtered["field_editors"][0]["id"], "directory_search");
    assert_eq!(
        filtered.directory("directory_shortcuts")["text"],
        "↵ Copy Address   ⇧↵ Message   ⎋ Clear Search"
    );
    let escape = json!({"op":"test_directory_key","key":"escape","via":"field"});
    f.send(p, escape.clone());
    let cleared = inspect(&f, p);
    assert_eq!(cleared["directory_open"], true);
    assert_eq!(cleared["directory_query"], "");
    assert_eq!(cleared.directory("directory_search")["text"], "");
    assert_eq!(
        cleared.directory("directory_shortcuts")["text"],
        "↵ Copy Address   ⇧↵ Message   ⎋ Close"
    );
    assert_eq!(
        cleared["field_editors"][0]["instance"],
        filtered["field_editors"][0]["instance"]
    );
    assert_eq!(latest(&f, p, "query_directory")["query"], "");
    assert_eq!(f.frames_for(p, "close_directory").count(), 0);

    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2}),
    );
    assert_eq!(inspect(&f, p)["directory_matched_count"], 2);
    f.send(p, escape);
    assert_eq!(inspect(&f, p)["directory_open"], false);
    assert_eq!(f.frames_for(p, "close_directory").count(), 1);
}

#[test]
fn correlation_editor_keys_and_modal_escape() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["harnesses"] = json!((0..6).map(|i| json!({"id":format!("agent-{i}"),"display_name":format!("Agent {i}"),"kind":"agent","sf_symbol":"terminal","tint":"blue","tier":"stable","order":i})).collect::<Vec<_>>());
    frame["my_remote_listeners"] = json!([peer("remote")]);
    f.send(p, frame);
    control(&f, p, "toggle_my_remote");
    control(&f, p, "copy_remote:personal:remote");
    assert_eq!(
        inspect(&f, p)["clipboard"],
        peer("remote")["display_address"]
    );
    control(&f, p, "open_directory");
    control(&f, p, "dir_harness:agent-5");
    let overflow = inspect(&f, p);
    assert_eq!(overflow["directory_harness"], "agent-5");
    assert!(!overflow.directory("directory_harness_scroll").is_null());
    control(&f, p, "dir_harness:all");
    let mut canonical = peer("one");
    canonical["listener_id"] = json!("c_abcd123456789012");
    canonical["public_id"] = json!("bob-abcd123456789012");
    canonical["display_address"] = json!("bob/fixture#abcd");
    send_fixture(&f, p, json!({"op":"directory","peers":[canonical]}));
    for prefix in ["abcd1234", "abcd12345678", "abcd123456789012"] {
        f.send(
            p,
            json!({"op":"test_directory_query","query":format!("bob/fixture#{prefix}")}),
        );
        assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    }
    f.send(p, json!({"op":"test_directory_query","query":""}));
    let before = inspect(&f, p);
    f.send(p,json!({"op":"directory","request_id":"stale","query":"","scope":"all","harness":"all","peers":[peer("two")],"online_count":0}));
    assert_eq!(
        inspect(&f, p)["directory_selected_id"],
        before["directory_selected_id"]
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one"),peer("two")]}),
    );
    control(&f, p, "dir_peer:personal:one");
    f.send(p, json!({"op":"test_focus_field","id":"directory_table"}));
    f.send(
        p,
        json!({"op":"test_directory_key","key":"down","via":"field"}),
    );
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    assert!(f.output(p, true).contains(r#"field-key {"handled":false}"#));
    control(&f, p, "msg_dir:personal:one");
    f.send(p, json!({"op":"test_focus_field","id":"composer_input"}));
    let selected = inspect(&f, p);
    assert!(selected["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|editor| editor["id"] == "composer_input"));
    for via in ["field", "handler"] {
        f.send(p, json!({"op":"test_directory_key","key":"down","via":via}));
        assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    }
    f.send(
        p,
        json!({"op":"test_directory_key","key":"command-w","via":"field"}),
    );
    assert_eq!(inspect(&f, p)["directory_open"], true);
    f.send(
        p,
        json!({"op":"test_directory_key","key":"return","via":"field"}),
    );
    assert_eq!(
        inspect(&f, p).directory("composer_error")["text"],
        "Enter a message to send."
    );
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["composer_open"], false);
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:hubs");
    control(&f, p, "edit_hub:personal");
    assert_eq!(inspect(&f, p).settings("submit_add_hub")["text"], "Save");
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert!(s.settings("add_hub_form").is_null());

    control(&f, p, "open_add_hub");
    f.send(p, json!({"op":"test_settings_input","id":"settings_hub_url","value":"https://id.example.com"}));
    f.send(p, json!({"op":"test_focus_field","id":"settings_hub_url"}));
    let s = inspect(&f, p);
    assert_eq!(s.settings("submit_add_hub")["text"], "Join Hub");
    assert!(s["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|editor| editor["id"] == "settings_hub_url"));
    control(&f, p, "open_uninstall_confirm");
    let modal = inspect(&f, p);
    assert_eq!(modal.settings("settings_hub_url")["enabled"], true);
    assert_eq!(modal.settings("settings_tab:apps")["enabled"], true);
    assert_eq!(
        modal.settings("cancel_uninstall_all")["native_button"],
        true
    );
    assert!(!modal["field_editors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|editor| editor["id"] == "settings_hub_url"));
    f.send(
        p,
        json!({"op":"test_clipboard","text":"https://changed.example.com"}),
    );
    f.send(p, json!({"op":"test_tab","surface":"settings"}));
    f.send(
        p,
        json!({"op":"test_settings_key","key":"command-v","via":"panel"}),
    );
    control(&f, p, "settings_tab:apps");
    let s = inspect(&f, p);
    assert_eq!(s["settings_tab"], "hubs");
    assert_eq!(
        s.settings("settings_hub_url")["text"],
        "https://id.example.com"
    );
    assert_eq!(modal.settings("uninstall_alert_sheet")["on_screen"], false);
    assert_eq!(modal.settings("settings_footer")["hidden"], false);
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    let s = inspect(&f, p);
    assert_eq!(s["settings_open"], true);
    assert!(s.settings("uninstall_alert_sheet").is_null());
    assert_eq!(f.frames_for(p, "setup_disconnect_all").count(), 0);
    assert_eq!(s.settings("settings_hub_url")["enabled"], true);
    f.send(p, json!({"op":"test_settings_key","key":"escape"}));
    assert!(inspect(&f, p).settings("add_hub_form").is_null());
}

#[test]
fn appkit_fragmented_utf8_invalid_frames_activity_and_eof() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let bytes = format!("{}\n", state()).into_bytes();
    let split = bytes.iter().position(|b| *b == 0xc3).unwrap() + 1;
    f.send_raw(p, &bytes[..split]);
    f.send_raw(p, &bytes[split..]);
    wait(|| f.output(p, true).contains("state hubs=2 listeners=1"));
    f.send_raw(p, b"bad json\n");
    // A valid projection larger than the former frame cap still reaches the UI.
    // Padding is an ignored extension field, so this tests transport, not text layout.
    let mut large = state();
    large["extension_data"] = json!("x".repeat(1_048_577));
    large["unread_count"] = json!(7);
    f.send(p, large);
    assert_eq!(inspect(&f, p)["unread_count"], 7);
    assert!(!f.exited(p));
    assert!(f.output(p, true).contains("invalid frame"));
    control(&f, p, "open_activity");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "open_directory" && v["mode"] == "activity")
    });
    assert_eq!(inspect(&f, p)["directory_mode"], "activity");
    f.eof(p);
    wait(|| f.exited(p));
    assert_eq!(f.frames_for(p, "quit").count(), 0);
}

#[test]
fn native_quit_sends_one_request_and_exits_if_the_daemon_does_not_close_the_pipe() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, json!({"op":"test_terminate"}));
    f.send(p, json!({"op":"test_terminate"}));
    // Leave stdin open to exercise the fallback, not the EOF path.
    wait(|| f.exited(p));
    f.stop(p);
    assert_eq!(
        f.frames_for(p, "quit").collect::<Vec<_>>(),
        vec![json!({"op":"quit"})]
    );
}

#[test]
fn native_quit_shuts_down_the_supervising_daemon_without_respawn() {
    use std::io::Write;
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};

    let _lock = serial();
    let f = Fixture::new(json!({}));
    let commands = f.home.join("ui-commands");
    let path = std::ffi::CString::new(commands.as_os_str().as_bytes()).unwrap();
    // Inject a menu gesture into the real headless child. Its stdout remains
    // attached directly to the daemon, and exec preserves the supervised PID.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let launcher = f.dir.join("native-ui.sh");
    script(
        &launcher,
        "#!/bin/sh\necho $$ >> \"$HOME/ui-pids\"\nexec 3<&0\nexec \"$TEST_UI\" < \"$HOME/ui-commands\"\n",
    );
    let staged_ui = root().join("bin/cito-ui-darwin-arm64");
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", launcher.to_str().unwrap()),
        (
            "TEST_UI",
            option_env!("CITO_TEST_BUILT_UI").unwrap_or_else(|| staged_ui.to_str().unwrap()),
        ),
    ]);
    let mut injection = None;
    wait(|| {
        injection = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&commands)
            .ok();
        injection.is_some()
    });
    let mut injection = injection.unwrap();
    writeln!(injection, "{}", json!({"op":"test_terminate"})).unwrap();
    // Keep the test input open; exit must come from the daemon or the fallback.
    wait(|| f.exited(daemon));
    let launches = fs::read_to_string(f.home.join("ui-pids")).unwrap();
    let pids: Vec<i32> = launches.lines().map(|line| line.parse().unwrap()).collect();
    assert_eq!(pids.len(), 1, "ordinary Quit must not respawn the UI");
    assert_ne!(
        unsafe { libc::kill(pids[0], 0) },
        0,
        "daemon left its UI running"
    );
}

#[test]
fn daemon_ui_action_stream_accepts_full_messages_and_recovers_after_oversized_lines() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let ui = f.dir.join("action-ui.py");
    script(
        &ui,
        r#"#!/usr/bin/env python3
import json, os, sys
sent = False
with open(os.environ['HOME'] + '/ui-frames', 'a') as out:
    for line in sys.stdin:
        out.write(line)
        out.flush()
        frame = json.loads(line)
        target = next((row['listener_id'] for row in frame.get('listeners', [])
                       if row.get('harness') == 'fixture'), None)
        if sent or not target:
            continue
        sent = True
        def action(message):
            return json.dumps({'op': 'send_signal', 'to': target, 'message': message},
                              ensure_ascii=False).encode('utf-8')
        output = sys.stdout.buffer
        output.write(action('😀' * 65_536) + b'\n')
        output.write(action('After maximum-size message') + b'\n')
        exact = action('Exact action limit')
        output.write(exact + b' ' * (1_048_576 - len(exact)) + b'\n')
        oversized = action('😀' * 262_144)
        # Force the bounded read to end inside a UTF-8 sequence.
        padding = (2 - oversized.index('😀'.encode('utf-8'))) % 4
        output.write(b' ' * padding + oversized + b'\n')
        output.write(action('After oversized action') + b'\n')
        output.flush()
"#,
    );
    let daemon = f.daemon(&[
        ("CITO_DISABLE_UI", "0"),
        ("CITO_UI_BINARY", ui.to_str().unwrap()),
    ]);
    f.tool(
        "cito_listen",
        json!({"about":"UI action receiver","scope":"workspace"}),
        meta,
    );
    let frames = || -> Vec<Value> {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    wait(|| {
        frames()
            .iter()
            .filter(|frame| frame["op"] == "composer_result")
            .count()
            == 4
    });
    let frames = frames();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "composer_result")
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
            json!({"op":"composer_result","ok":true,"error":null,"request_id":null}),
        ]
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["op"] == "error")
            .cloned()
            .collect::<Vec<_>>(),
        vec![json!({"op":"error","error":"Message is too large to send"})]
    );
    assert_eq!(
        f.db("hub.sqlite")
            .query(
                "SELECT json_extract(CAST(data AS TEXT),'$.message') AS message FROM hub_signals ORDER BY seq",
                &[],
            )
            .unwrap(),
        vec![
            json!({"message":"😀".repeat(65_536)}),
            json!({"message":"After maximum-size message"}),
            json!({"message":"Exact action limit"}),
            json!({"message":"After oversized action"}),
        ]
    );
    assert!(!f.exited(daemon));
}
#[test]
fn daemon_owns_headless_ui_child_and_reaps_it_on_shutdown() {
    let _lock = serial();
    let f = Fixture::new(json!({"hubs":{"local":{"url":"local://","autoListen":false}}}));
    let daemon = f.recording_daemon();
    wait(|| f.home.join("ui-pid").exists());
    f.action(json!({"op":"open_settings"}));
    wait(|| {
        fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .contains("settings")
    });
    let pid = fs::read_to_string(f.home.join("ui-pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    f.action(json!({"op":"open_menu"}));
    f.action(json!({"op":"open_directory"}));
    // A crash closes the connection and replacement starts with every surface
    // closed. An external mutation then emits status, never a full inventory.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    let mut replacement = pid;
    wait(|| {
        replacement = fs::read_to_string(f.home.join("ui-pid"))
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(pid);
        replacement != pid
    });
    wait(|| {
        !fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .is_empty()
    });
    f.db("state.sqlite").execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,hub_url,principal_id) SELECT hub,'crash-state',listener_id,1,1,'bob','host','Sender','Work','now',?,'RECEIVED',hub_url,principal_id FROM local_listeners WHERE hub='local' AND harness='human' AND status='ACTIVE'", &[json!(cito_native::plugins::now_ms()+60_000)]).unwrap();
    let mut frame = Value::Null;
    wait(|| {
        frame = fs::read_to_string(f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .rfind(|v| v["op"] == "state" && v["unread_count"] == 1)
            .unwrap_or(Value::Null);
        !frame.is_null()
    });
    assert!(frame.get("settings").is_none(), "{frame}");
    assert!(frame.get("activity").is_none(), "{frame}");
    f.stop(daemon);
    wait(|| unsafe { libc::kill(replacement, 0) } != 0);
}
