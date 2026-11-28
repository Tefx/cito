use super::settlement::latest;
use super::*;

#[test]
fn activity_reply_blocks_cross_recipient_drafts_before_acknowledging() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["activity"] = json!([("one", "alice"), ("two", "bob")].map(|(id, user)| {
        json!({"hub":"personal","signal_id":id,"listener_id":"c_human","from_user":user,"from_host_name":"laptop",
            "from_harness":"fixture","from_title":user,"from_scoped_id":format!("personal:{id}"),
            "target_harness":"human","target_title":"Me","target_scoped_id":"personal:me",
            "message":"Review?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true})
    }));
    frame["unread_count"] = json!(2);
    f.send(p, frame);
    f.send(
        p,
        json!({"op":"test_open_panel","surface":"directory","mode":"activity"}),
    );
    control(&f, p, "reply_activity:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"A draft for Alice"}),
    );
    let draft = inspect(&f, p);
    assert_eq!(draft["composer_target"], "personal:one");
    control(&f, p, "reply_activity:two");
    let blocked = inspect(&f, p);
    assert_eq!(blocked["composer_reply_signal_id"], "one");
    assert_eq!(
        blocked.directory("composer_input")["text"],
        "A draft for Alice"
    );
    let acks: Vec<_> = f.frames_for(p, "ack_signals").collect();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0]["signal_ids"], json!(["one"]));
    control(&f, p, "composer_cancel");
    control(&f, p, "reply_activity:two");
    assert_eq!(inspect(&f, p)["composer_target"], "personal:two");
    assert_eq!(latest(&f, p, "ack_signals")["signal_ids"], json!(["two"]));
}

#[test]
fn v41_directory_copy_drafts_and_activity_confirmation_survive_only_valid_transitions() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    f.send(p, frame.clone());
    control(&f, p, "open_directory");
    f.send(
        p,
        json!({"op":"test_directory_query","query":"missing-person"}),
    );
    let filtered = inspect(&f, p);
    assert_eq!(filtered["directory_matched_count"], 0);
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["directory_matched_count"], 0);
    f.send(p, json!({"op":"test_directory_query","query":""}));
    inspect(&f, p);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":2}),
    );
    control(&f, p, "copy_dir:personal:one");
    inspect(&f, p);
    frame["harnesses"][0]["tint"] = json!("teal");
    f.send(p, frame);
    assert_eq!(
        inspect(&f, p).directory("copy_dir:personal:one")["text"],
        "✓ Copied"
    );
    control(&f, p, "copy_dir:personal:two");
    f.send(
        p,
        json!({"op":"test_expire_copy_feedback","id":"copy_dir:personal:one"}),
    );
    let expired = inspect(&f, p);
    assert_eq!(
        expired.directory("copy_dir:personal:one")["text"],
        peer("one")["display_address"]
    );
    assert_eq!(
        expired.directory("copy_dir:personal:two")["text"],
        "✓ Copied"
    );
    assert!(expired["directory_copy_remaining"]["personal:one"].is_null());
    assert!(
        expired["directory_copy_remaining"]["personal:two"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Keep this draft"}),
    );
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    f.send(p, json!({"op":"test_directory_key","key":"shift-return"}));
    let draft = inspect(&f, p);
    assert_eq!(draft["composer_target"], peer("one")["scoped_id"]);
    assert_eq!(draft.directory("composer_input")["text"], "Keep this draft");
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity_filter":"all","activity_total":1,"activity":[{"hub":"personal","signal_id":"observed","listener_id":"agent","from_user":"bob","from_host_name":"desktop","from_title":"Build","target_harness":"fixture","target_title":"Session","target_scoped_id":"personal:agent","message":"Build ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":false}]}),
    );
    control(&f, p, "activity_clear");
    assert!(!inspect(&f, p)
        .directory("activity_clear_confirm_commit")
        .is_null());
    f.send(p, json!({"op":"test_directory_query","query":"Build"}));
    assert!(inspect(&f, p)
        .directory("activity_clear_confirm_commit")
        .is_null());
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    inspect(&f, p);
    assert!(f.frames_for(p, "ack_signals").next().is_none());
}

#[test]
fn directory_copy_expiry_restores_every_control_observing_the_same_peer() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_directory");
    let one = peer("one");
    let two = peer("two");
    send_fixture(&f, p, json!({"op":"directory","peers":[one,two]}));
    let id = "copy_dir:personal:one";
    control(&f, p, id);
    assert_eq!(inspect(&f, p).directory(id)["text"], "✓ Copied");

    // Project the same peer in two cells to force overlapping subscriptions
    // without relying on which cell AppKit chooses to recycle during the reload.
    send_fixture(&f, p, json!({"op":"directory","peers":[one,one,two]}));
    let peer_controls = |snapshot: &Value| {
        nodes(&snapshot["directory_items"])
            .into_iter()
            .filter(|node| node["id"] == id)
            .collect::<Vec<_>>()
    };
    let copied = peer_controls(&inspect(&f, p));
    assert_eq!(copied.len(), 2);
    assert_ne!(copied[0]["instance"], copied[1]["instance"]);
    assert!(copied.iter().all(|node| node["text"] == "✓ Copied"));

    control(&f, p, "copy_dir:personal:two");
    f.send(p, json!({"op":"test_expire_copy_feedback","id":id}));
    let expired = inspect(&f, p); // Also requires address_mismatches == [].
    let restored = peer_controls(&expired);
    assert_eq!(restored.len(), 2);
    for (before, after) in copied.iter().zip(restored) {
        assert_eq!(after["instance"], before["instance"]);
        assert_eq!(after["text"], one["display_address"]);
        assert_eq!(
            after["accessibility_label"],
            format!("Copy address {}", one["display_address"].as_str().unwrap())
        );
    }
    assert_eq!(
        expired.directory("copy_dir:personal:two")["text"],
        "✓ Copied"
    );
}

#[test]
fn inbox_scope_gating_read_all_search_and_confirmed_empty_directory() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state_with_settings(settings());
    frame["hubs"][0]["human_listener_id"] = json!("inbox");
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_scope"] = json!("workspace");
    frame["settings"]["hubs"][0]["human_scope"] = json!("workspace");
    frame["settings"]["hubs"][0]["human_state_status"] = json!("current");
    frame["settings"]["hubs"][0]["human_listener_id"] = json!("inbox");
    frame["settings"]["hubs"][0]["human_can_set_scope"] = json!(true);
    frame["settings"]["hubs"][0]["supports_workspace_scope"] = json!(true);
    let mut human = listener();
    human["harness"] = json!("human");
    human["listener_id"] = json!("inbox");
    human["scoped_id"] = json!("personal:inbox");
    human["public_id"] = json!("inbox");
    human["display_address"] = json!("@alice");
    frame["listeners"].as_array_mut().unwrap().push(human);
    frame["listeners"][0]["scope"] = json!("workspace");
    let agent_id = frame["listeners"][0]["scoped_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let full_address = format!("{}/fixture#abcd", "long-account-".repeat(8));
    frame["listeners"][0]["display_address"] = json!(full_address);
    f.send(p, frame.clone());
    let copy_id = format!("copy:{agent_id}");
    let compact = inspect(&f, p).menu(&copy_id)["text"].clone();
    assert_ne!(compact, full_address);
    control(&f, p, &copy_id);
    inspect(&f, p);
    f.send(p, json!({"op":"test_expire_copy_feedback","id":copy_id}));
    let restored = inspect(&f, p);
    assert_eq!(restored.menu(&copy_id)["text"], compact);
    assert_eq!(restored.menu(&copy_id)["accessibility_label"], full_address);
    for mode in ["oidc", "token"] {
        frame["hubs"][0]["auth_mode"] = json!(mode);
        f.send(p, frame.clone());
        control(&f, p, "open_directory");
        send_fixture(
            &f,
            p,
            json!({"op":"directory","peers":if mode=="token"{vec![peer("other")]}else{vec![]},"matched_count":if mode=="token"{1}else{0},"online_count":1}),
        );
        control(&f, p, "open_settings");
        control(&f, p, "settings_tab:channels");
        let s = inspect(&f, p);
        let inbox = s.settings("scope:inbox");
        let agent = s.menu(&format!("scope:{agent_id}"));
        assert_eq!(inbox["text"], "Team", "{mode}");
        assert_eq!(inbox["symbol"], "person.2.fill");
        assert_eq!(inbox["background"], agent["background"]);
        assert!(inbox["tooltip"]
            .as_str()
            .unwrap()
            .contains("Visibility: Team"));
    }
    // A confirmed empty reply owns the result even when state still contains seeds.
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"matched_count":0,"online_count":0}),
    );
    control(&f, p, "close_directory");
    control(&f, p, "open_directory");
    f.send(p, frame.clone());
    let empty = inspect(&f, p);
    assert_eq!(empty["directory_selected_id"], "");
    assert_eq!(empty["directory_matched_count"], 0);

    frame["unread_count"] = json!(2);
    frame["activity"] = json!([("review","Review ready"),("build","Build passed")].into_iter().map(|(id,message)| json!({"hub":"personal","signal_id":id,"listener_id":"inbox","from_user":"bob","from_host_name":"desktop","from_title":"Sender","target_harness":"human","target_title":"Human","target_scoped_id":"personal:inbox","message":message,"created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p)["unread_count"], 2);
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    f.send(
        p,
        json!({"op":"test_directory_query","query":"Review ready"}),
    );
    // The server counted both matching messages, even though only one row is cached.
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[frame["activity"][0]],"activity_filter":"all","activity_query":"Review ready","activity_total":2,"activity_unread_count":2,"unread_count":3}),
    );
    control(&f, p, "activity_mark_all_read");
    let ack = latest(&f, p, "ack_signals");
    assert_eq!(ack["ack_all"], true);
    assert_eq!(ack["query"], "Review ready");
    f.send(p, json!({"op":"error","source_op":"ack_signals","request_id":ack["request_id"],"error":"Hub unavailable"}));
    let failed = inspect(&f, p);
    assert_eq!(
        failed.directory("directory_warning_label")["tooltip"],
        "Hub unavailable"
    );
    assert_eq!(failed["unread_count"], 3);
    assert_eq!(
        failed.directory("directory_summary")["text"],
        "2 messages · 2 unread"
    );
    assert_eq!(failed.directory("unread_accent_bar")["hidden"], false);
    assert_eq!(failed.directory("activity_mark_all_read")["enabled"], true);
    f.send(
        p,
        json!({"op":"test_directory_query","query":"Build passed"}),
    );
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[frame["activity"][1]],"activity_filter":"all","activity_query":"Build passed","activity_total":1,"activity_unread_count":1}),
    );
    let remaining = inspect(&f, p);
    assert_eq!(
        remaining.directory("activity_mark_all_read")["enabled"],
        true
    );

    let mut cfg = settings();
    cfg["hubs"][0]["human_paused"] = json!(true);
    cfg["hubs"][0]["human_state_status"] = json!("current");
    cfg["hubs"][0]["status"] = json!("disconnected");
    f.send(p, json!({"op":"settings","settings":cfg}));
    control(&f, p, "open_settings");
    control(&f, p, "settings_tab:channels");
    let paused = inspect(&f, p);
    assert!(paused.settings("inbox_control_bar").is_object());
    assert_eq!(paused.settings("toggle_inbox")["enabled"], false);
    control(&f, p, "open_uninstall_confirm");
    control(&f, p, "uninstall_purge_data");
    let purge = inspect(&f, p);
    assert!(purge.settings("confirm_disconnect_all")["tooltip"]
        .as_str()
        .unwrap()
        .contains("delete local data and Hub settings"));
}

#[test]
fn v39_reload_hint_retains_full_tooltip_and_activity_reset_requeries() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["harnesses"][0]["display_name"] = json!("Claude Code");
    frame["available"] = json!([{"harness":"fixture","session_id":"reload","title":"Review","cwd":"/tmp","needs_reload":true}]);
    frame["activity_has_any"] = json!(true);
    f.send(p, frame);
    control(&f, p, "toggle_available");
    let hint = inspect(&f, p).menu("candidate_hint:fixture:reload");
    assert_eq!(
        hint["tooltip"],
        "Run /reload in Claude Code to enable 1-click listen"
    );
    assert_eq!(hint["text"], hint["tooltip"]);
    assert_eq!(hint["width"], 244);
    assert_eq!(hint["x"], 46);
    control(&f, p, "open_activity");
    f.send(p, json!({"op":"test_directory_query","query":"missing"}));
    assert!(!inspect(&f, p).directory("act_reset_search").is_null());
    control(&f, p, "act_reset_search");
    let s = inspect(&f, p);
    assert_eq!(s.directory("directory_search")["text"], "");
    let query = f.frames_for(p, "query_directory").next_back().unwrap();
    assert_eq!(query["query"], "");
    assert_eq!(query["mode"], "activity");
    control(&f, p, "act_filter:me");
    assert!(!inspect(&f, p).directory("act_empty_show_all").is_null());
    control(&f, p, "act_empty_show_all");
    let s = inspect(&f, p);
    assert_eq!(s["activity_filter"], "all");
    let query = f.frames_for(p, "query_directory").next_back().unwrap();
    assert_eq!(query["filter"], "all");
}

#[test]
fn appkit_directory_human_labels_normalize_legacy_defaults_and_preserve_custom_details() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    send_companion(&f, p, state());
    control(&f, p, "open_directory");
    for (title, about, expected_title, expected_about) in [
        ("", "Human", "@bob", ""),
        ("Human", "Human", "@bob", ""),
        ("@bob", "@bob", "@bob", ""),
        ("@bob (Me)", "@bob (Me)", "@bob", ""),
        ("Human Node (@old)", "Human Node (@old)", "@bob", ""),
        ("Review desk", "Review desk", "Review desk", ""),
        (
            "Review desk",
            "Release approvals",
            "Review desk",
            "Release approvals",
        ),
    ] {
        let mut human = peer("human");
        human["harness"] = json!("human");
        human["display_address"] = json!("@bob");
        human["title"] = json!(title);
        human["custom_title"] = json!(title == "Review desk");
        human["about"] = json!(about);
        human["attachments"] = json!([{"channel_id":"remote:human","location":"device","host_id":"remote","display_name":"laptop","availability":"ACTIVE","is_local":false}]);
        let mut agent = peer("agent");
        agent["title"] = json!("Human");
        agent["about"] = json!("Human");
        send_fixture(&f, p, json!({"op":"directory","peers":[human,agent]}));
        let s = inspect(&f, p);
        assert_eq!(
            s.directory("dir_title:personal:human")["text"],
            expected_title
        );
        assert_eq!(
            s.directory("dir_meta:personal:human")["text"],
            if expected_about.is_empty() {
                "Inbox · laptop".into()
            } else {
                format!("Inbox · laptop · {expected_about}")
            }
        );
        assert_eq!(s.directory("dir_title:personal:agent")["text"], "Fixture");
        assert_eq!(s.directory("dir_meta:personal:agent")["text"], " · laptop");
    }
    let mut frame = state();
    frame["hubs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"local","status":"connected","auth_mode":"none","user":"bob"}));
    send_companion(&f, p, frame.clone());
    let mut agent = peer("home");
    agent["session_id"] = json!("home-session");
    agent["title"] = json!("bob");
    agent["about"] = json!("Release audit");
    agent["cwd"] = json!("~");
    agent["repo"] = json!("");
    agent["branch"] = json!("");
    agent["host_name"] = json!("laptop.LOCAL");
    agent["channels"] = json!([
        "Mac",
        "Hub",
        "human",
        "Desktop notifications",
        "laptop.local",
        "mac-studio.local",
        "mac-studio"
    ]);
    frame["my_remote_listeners"] = json!([agent.clone()]);
    send_companion(&f, p, frame);
    control(&f, p, "toggle_my_remote");
    let remote = inspect(&f, p).menu("remote:personal:home");
    let remote_info = nodes(&remote["children"])
        .into_iter()
        .find(|n| n["text"] == "laptop · Release audit")
        .unwrap();
    assert_eq!(remote_info["tooltip"], agent["display_address"]);
    let mut shadow = agent.clone();
    shadow["hub"] = json!("local");
    shadow["scoped_id"] = json!("local:home");
    shadow["display_address"] = json!(format!(
        "local:{}",
        shadow["display_address"].as_str().unwrap()
    ));
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[shadow,agent.clone()]}),
    );
    let s = inspect(&f, p);
    assert_eq!(
        s.directory("dir_title:personal:home")["text"],
        "Release audit"
    );
    assert_eq!(
        s.directory("dir_meta:personal:home")["text"],
        " · laptop, mac-studio"
    );
    assert_eq!(s.directory("dir_repo:personal:home")["text"], " · ~");
    assert!(!s.directory("dir_title:local:home").is_null());
    assert_eq!(s["directory_matched_count"], 2);
    assert_eq!(s["directory_online_count"], 2);
    agent["about"] = json!("");
    send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
    assert_eq!(
        inspect(&f, p).directory("dir_title:personal:home")["text"],
        "Fixture"
    );
    // Foundation resolves the account home independently of the fixture's HOME.
    let home_basename = inspect(&f, p)["home_basename"].as_str().unwrap().to_owned();
    let at_home_basename = format!("@{home_basename}");
    // Description aliases share the daemon/UI process HOME, not the account database.
    let description_home = f.home.file_name().unwrap().to_str().unwrap();
    let at_description_home = format!("@{description_home}");
    for raw in [
        "bob",
        "@bob",
        home_basename.as_str(),
        at_home_basename.as_str(),
        "Fixture",
        "",
        "Agent",
        "Agent session",
        "Session",
        "CLI session",
        "Human",
        "Human node",
        "Human channel",
    ] {
        for (about, expected) in [
            ("CLI session", "Fixture"),
            ("bob", "Fixture"),
            ("@bob", "Fixture"),
            (description_home, "Fixture"),
            (at_description_home.as_str(), "Fixture"),
            (
                "account",
                if description_home.eq_ignore_ascii_case("account") {
                    "Fixture"
                } else {
                    "account"
                },
            ),
            (
                "@account",
                if description_home.eq_ignore_ascii_case("account") {
                    "Fixture"
                } else {
                    "@account"
                },
            ),
        ] {
            let expected = if raw == "Fixture" {
                raw
            } else if about.to_lowercase() == raw.to_lowercase() {
                "Fixture"
            } else {
                expected
            };
            agent["title"] = json!(raw);
            agent["about"] = json!(about);
            agent["cwd"] = json!(f.home);
            send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
            assert_eq!(
                inspect(&f, p).directory("dir_title:personal:home")["text"],
                expected,
                "raw={raw}, about={about}"
            );
            if about == description_home || about == at_description_home {
                assert_eq!(
                    inspect(&f, p).directory("dir_meta:personal:home")["tooltip"],
                    " · laptop, mac-studio"
                );
            }
        }
    }
    if home_basename.to_lowercase() != description_home.to_lowercase()
        && ![
            "bob",
            "@bob",
            "you (@bob)",
            "fixture",
            "human",
            "human node",
            "human channel",
            "human receiver",
            "you",
            "session",
            "coding session",
            "default",
            "agent",
            "agent session",
            "cli session",
        ]
        .contains(&home_basename.to_lowercase().as_str())
    {
        agent["title"] = json!("bob");
        agent["about"] = json!(home_basename);
        send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
        assert_eq!(
            inspect(&f, p).directory("dir_title:personal:home")["text"],
            home_basename,
            "A name outside this process's HOME and identity aliases remains distinct"
        );
    }
    agent["title"] = json!("Auth refactor");
    agent["custom_title"] = json!(false);
    for cwd in [json!("~"), json!(f.home)] {
        agent["cwd"] = cwd;
        for about in ["Auth refactor", "", "CLI session"] {
            agent["about"] = json!(about);
            send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
            let s = inspect(&f, p);
            assert_eq!(
                s.directory("dir_title:personal:home")["text"],
                "Auth refactor"
            );
            assert!(!s.directory("dir_meta:personal:home")["tooltip"]
                .as_str()
                .unwrap()
                .contains("Auth refactor"));
        }
    }
    agent["custom_title"] = json!(true);
    for title in ["bob", "Human"] {
        agent["title"] = json!(title);
        send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
        assert_eq!(
            inspect(&f, p).directory("dir_title:personal:home")["text"],
            title
        );
    }
    control(&f, p, "open_activity");
    control(&f, p, "act_filter:all");
    for (title, expected) in [
        ("@bob (Me)", "@bob → Agent"),
        (" @BOB (mE) ", "@bob → Agent"),
        ("Release review", "@bob (Release review) → Agent"),
    ] {
        let mut frame = state();
        frame["activity"] = json!([{"hub":"personal","signal_id":"legacy-human","listener_id":"target","from_user":"bob","from_host_name":"laptop","from_harness":"human","from_title":title,"target_harness":"fixture","target_title":"Agent","target_scoped_id":"personal:target","message":"Review complete","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":false}]);
        send_companion(&f, p, frame);
        assert_eq!(
            inspect(&f, p).directory("act_title:legacy-human")["text"],
            expected
        );
    }
}

#[test]
fn activity_projection_is_atomic_and_only_unread_human_replies_acknowledge() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["activity_has_any"] = json!(true);
    f.send(p, frame.clone());
    control(&f, p, "open_activity");
    control(&f, p, "act_filter:all");
    let item = |id, target, unread| {
        json!({"hub":"personal","signal_id":id,"listener_id":"c_target","from_user":"bob",
            "from_host_name":"laptop","from_harness":"fixture","from_title":"Schema work",
            "from_scoped_id":"personal:sender","target_harness":target,"target_title":"Review",
            "target_scoped_id":"personal:target","message":"Approve?","created_at":"2030-01-01T00:00:00Z",
            "unread":unread,"for_me":true,"can_reply":true})
    };
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[
        item("agent-unread", "fixture", true), item("human-read", "human", false),
        item("human-unread", "human", true)], "activity_total":18,"activity_unread_count":9,"activity_filter":"all"}),
    );
    let summary = |snapshot: &Value| snapshot.directory("directory_summary")["text"].clone();
    assert_eq!(summary(&inspect(&f, p)), "18 messages · 9 unread");

    frame["unread_count"] = json!(4);
    f.send(p, frame);
    let preserved = inspect(&f, p);
    assert_eq!(preserved["unread_count"], 4);
    assert_eq!(summary(&preserved), "18 messages · 9 unread");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity_total":55,"activity_filter":"me","activity_query":"foreign slice"}),
    );
    assert_eq!(summary(&inspect(&f, p)), "18 messages · 9 unread");

    for id in ["agent-unread", "human-read", "human-unread"] {
        control(&f, p, &format!("reply_activity:{id}"));
        assert_eq!(inspect(&f, p)["composer_reply_signal_id"], id);
        control(&f, p, "composer_cancel");
    }
    inspect(&f, p);
    let acks: Vec<_> = f.frames_for(p, "ack_signals").collect();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0]["signal_ids"], json!(["human-unread"]));
}

#[test]
fn appkit_activity_delete_cancels_a_press_when_a_correlated_refresh_reuses_the_row() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_activity");
    control(&f, p, "act_filter:all");
    let activity = |ids: [&str; 3]| {
        ids.map(|id| json!({"hub":"personal","signal_id":id,
        "listener_id":"c_target","from_user":"bob","from_host_name":"laptop","from_harness":"fixture",
        "from_title":"Schema work","target_harness":"human","target_title":"Review","message":"Approve?",
        "created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true,"can_reply":true}))
    };
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":activity(["a", "b", "c"]),"activity_filter":"all"}),
    );
    let original = inspect(&f, p);
    assert_eq!(original["directory_selected_index"], 0);
    assert!(original.directory("delete_activity:a").is_object());
    let request = latest(&f, p, "query_directory");
    let mut refresh = json!({"op":"directory","peers":[],"activity":activity(["b", "c", "d"]),"activity_filter":"all"});
    for key in ["request_id", "query", "scope", "harness"] {
        refresh[key] = request[key].clone();
    }
    f.send(
        p,
        json!({"op":"test_tracked_row_press","id":"delete_activity:a","refresh":refresh}),
    );
    wait(|| f.output(p, true).contains("tracked-row-release"));
    let refreshed = inspect(&f, p);
    assert!(refreshed.directory("delete_activity:a").is_null());
    assert!(refreshed.directory("delete_activity:d").is_object());
    assert!(
        f.output(p, true)
            .contains("tracked-row-press true false activity_row:b"),
        "{}",
        f.output(p, true)
            .lines()
            .find(|line| line.contains("tracked-row-press"))
            .unwrap_or("no tracked press")
    );
    assert_eq!(f.frames_for(p, "clear_activity_history").count(), 0);
    // Exercise the incremental keyboard path after the replacement, then release inside B normally.
    let reloads = super::lazy_rendering::render_state(&f, p)["directory_reloads"].clone();
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(inspect(&f, p)["directory_selected_index"], 1);
    assert_eq!(
        super::lazy_rendering::render_state(&f, p)["directory_reloads"],
        reloads
    );
    f.send(
        p,
        json!({"op":"test_tracked_row_press","id":"delete_activity:b"}),
    );
    wait(|| f.frames_for(p, "clear_activity_history").count() == 1);
    assert_eq!(latest(&f, p, "clear_activity_history")["signal_id"], "b");
}

#[test]
fn appkit_activity_reply_filters_navigation_and_read_all() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    let activity = json!([{"hub":"personal","signal_id":"personal:sig_unread","listener_id":"c_human","from_user":"bob","from_host_name":"laptop","from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Approve?","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true},{"hub":"personal","signal_id":"personal:sig_read","listener_id":"c_other","from_user":"alice","from_host_name":"desktop","from_harness":"fixture","from_title":"Other","from_scoped_id":"personal:two","target_harness":"fixture","target_title":"Other","target_scoped_id":"personal:other","message":"Done","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":false}]);
    let mut activity = activity;
    activity[1]["message"] =
        json!("First line\nSecond line\nThird line\nFourth line\nFifth line\nSixth line");
    frame["activity"] = activity.clone();
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    control(&f, p, "open_activity");
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "activity");
    assert!(
        (s.directory("activity_filter_track")["background"][3]
            .as_f64()
            .unwrap()
            - 0.06)
            .abs()
            < 0.001
    );
    let collapsed = s.directory("act_msg:personal:sig_unread");
    assert_eq!(
        collapsed["y"].as_f64().unwrap() + collapsed["height"].as_f64().unwrap(),
        26.0
    );
    assert_eq!(s.directory("act_filter:unread")["border_width"], 0);
    assert_eq!(s.directory("act_filter:me")["border_width"], 1);
    assert_eq!(s.directory("activity_mark_all_read")["border_width"], 1);
    assert_eq!(s.directory("activity_clear")["border_width"], 1);
    assert_eq!(
        s.directory("directory_shortcuts")["text"],
        "↵ Reply    ⎋ Close"
    );
    let row = s.directory("activity_row:personal:sig_unread");
    assert_eq!(row["height"], 60);
    let children = nodes(&row["children"]);
    let accent = children
        .iter()
        .find(|n| n["id"] == "unread_accent_bar")
        .unwrap();
    assert_eq!(accent["hidden"], false);
    assert_eq!(accent["width"], 3);
    assert_eq!(accent["x"], 4);
    assert_eq!(accent["height"], 44);
    assert!(!children.iter().any(|n| n["id"] == "online_presence_dot"));
    assert_eq!(
        s.directory("act_time:personal:sig_unread")["text"],
        "Just now"
    );
    assert_eq!(s.directory("act_time:personal:sig_unread")["tooltip"], "");
    let viewport = s.directory("directory_scroll")["content_width"]
        .as_f64()
        .unwrap();
    assert_eq!(
        s.directory("act_msg:personal:sig_unread")["width"]
            .as_f64()
            .unwrap(),
        viewport - 68.0
    );
    let delete = s.directory("delete_activity:personal:sig_unread");
    assert_eq!(
        delete["x"].as_f64().unwrap() + delete["width"].as_f64().unwrap(),
        viewport - 16.0
    );
    assert_eq!(delete["symbol"], "xmark");
    assert_eq!(delete["width"], 24);
    assert_eq!(delete["tooltip"], "Delete message");
    control(&f, p, "delete_activity:personal:sig_unread");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "clear_activity_history"
                && v["hub"] == "personal"
                && v["signal_id"] == "personal:sig_unread"
        })
    });
    let deletions = f.frames_for(p, "clear_activity_history").count();
    f.send(p, json!({"op":"test_focus_field","id":"directory_search"}));
    f.send(p, json!({"op":"test_directory_key","key":"delete"}));
    inspect(&f, p);
    assert_eq!(
        f.frames_for(p, "clear_activity_history").count(),
        deletions,
        "Delete must keep normal text editing semantics"
    );
    control(&f, p, "act_filter:all");
    let short_row = "activity_row:personal:sig_unread";
    let long_row = "activity_row:personal:sig_read";
    let long_message = "act_msg:personal:sig_read";
    let before = inspect(&f, p);
    assert_eq!(before.directory(short_row)["selected"], true);
    assert_eq!(before.directory(long_row)["selected"], false);
    assert_eq!(before.directory(long_row)["row_height"], 60);
    assert_eq!(before.directory(long_message)["maximum_lines"], 1);
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "personal:sig_read");
    assert_eq!(s.directory(short_row)["selected"], false);
    assert_eq!(s.directory(long_row)["selected"], true);
    assert!(s.directory(long_row)["row_height"].as_f64().unwrap() > 60.0);
    assert_eq!(
        s.directory(long_row)["height"],
        s.directory(long_row)["row_height"]
    );
    assert_eq!(s.directory(long_message)["maximum_lines"], 4);
    assert_eq!(s.directory(long_message)["text"], activity[1]["message"]);
    for (style, raw_style) in [("overlay", 1), ("legacy", 0)] {
        f.send(p, json!({"op":"test_tile_scroll_views","style":style}));
        let tiled = inspect(&f, p);
        let scroll = tiled.directory("directory_scroll");
        assert_eq!(scroll["scroller_style"], raw_style);
        let preview = tiled.directory(long_message);
        assert_eq!(
            preview["width"].as_f64().unwrap(),
            scroll["content_width"].as_f64().unwrap() - 68.0
        );
        assert_eq!(tiled.directory(long_row)["row_height"], 108);
        assert_eq!(preview["maximum_lines"], 4);
        assert_eq!(preview["truncates_last_visible_line"], true);
        assert_eq!(preview["tooltip"], activity[1]["message"]);
    }
    // Directory has not replied yet. Updating the seed peers must not change Activity selection.
    frame["listeners"] = json!([]);
    f.send(p, frame);
    let seeded = inspect(&f, p);
    assert_eq!(seeded["directory_selected_id"], "personal:sig_read");
    assert_eq!(seeded.directory(long_row)["selected"], true);

    let mut inserted = activity.clone();
    let mut newer = activity[0].clone();
    newer["signal_id"] = json!("newer");
    inserted.as_array_mut().unwrap().insert(0, newer);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":inserted,"activity_filter":"all"}),
    );
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["directory_selected_id"], "personal:sig_read");
    assert_eq!(refreshed["directory_selected_index"], 2);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"activity":activity,"activity_filter":"all"}),
    );
    assert_eq!(inspect(&f, p)["directory_selected_index"], 1);
    f.send(p, json!({"op":"test_focus_field","id":"directory_table"}));
    f.send(p, json!({"op":"test_key_event","surface":"directory","character":"\u{7f}","key_code":51,"repeat":true}));
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "clear_activity_history").count(), deletions);
    f.send(
        p,
        json!({"op":"test_key_event","surface":"directory","character":"\u{7f}","key_code":51}),
    );
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "clear_activity_history"
                && v["hub"] == "personal"
                && v["signal_id"] == "personal:sig_read"
        })
    });
    assert!(s.directory("reply_activity:personal:sig_read")["hidden"] == true);
    control(&f, p, "reply_activity:personal:sig_read");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["composer_open"], false);
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    let collapsed = inspect(&f, p);
    assert_eq!(collapsed["directory_selected_id"], "personal:sig_unread");
    assert_eq!(collapsed.directory(short_row)["selected"], true);
    assert_eq!(collapsed.directory(long_row)["selected"], false);
    assert_eq!(collapsed.directory(long_row)["row_height"], 60);
    assert_eq!(collapsed.directory(long_row)["height"], 60);
    assert_eq!(collapsed.directory(long_message)["maximum_lines"], 1);
    control(&f, p, "act_filter:unread");
    assert!(inspect(&f, p)
        .directory("activity_row:personal:sig_read")
        .is_null());
    control(&f, p, "reply_activity:personal:sig_unread");
    let s = inspect(&f, p);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:sig_unread");
    assert_eq!(
        s.directory("directory_shortcuts")["text"],
        "↵ Send    ⎋ Cancel Reply"
    );
    f.send(p, json!({"op":"test_composer_input","value":"Approved"}));
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "send_signal"
                && v["in_reply_to"] == "personal:personal:sig_unread"
                && v["message"] == "Approved"
        })
    });
    control(&f, p, "composer_cancel");
    let queries = f.frames_for(p, "query_directory").count();
    control(&f, p, "activity_mark_all_read");
    let pending = inspect(&f, p);
    assert_eq!(pending["unread_count"], 1);
    assert_eq!(
        pending.directory("directory_summary")["text"],
        "1 message · 1 unread"
    );
    assert_eq!(f.frames_for(p, "query_directory").count(), queries + 1);
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "ack_signals" && v["ack_all"] == true)
    });
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[],"activity_filter":"unread","activity_total":0,"activity_unread_count":0,"unread_count":0}),
    );
    let confirmed = inspect(&f, p);
    assert_eq!(confirmed["unread_count"], 0);
    assert_eq!(
        confirmed.directory("directory_summary")["text"],
        "0 messages · 0 unread"
    );
    assert!(confirmed
        .directory("activity_row:personal:sig_unread")
        .is_null());
    control(&f, p, "act_filter:all");
    for (reason, label) in [
        ("stale_generation", "App reloaded"),
        ("driver_unavailable", "App unavailable"),
        ("session_switched", "Conversation changed"),
        ("unlistened", "Listening paused"),
        ("unlistened_before_dispatch", "Listening paused"),
        ("fenced_before_dispatch", "Connection changed"),
        ("sender_ack_timeout", "Awaiting acknowledgment"),
    ] {
        let mut item = activity[0].clone();
        item["held_reason"] = json!(reason);
        send_fixture(
            &f,
            p,
            json!({"op":"directory","peers":[],"activity":[item],"activity_filter":"all"}),
        );
        let snapshot = inspect(&f, p);
        assert_eq!(
            snapshot.directory("act_held:personal:sig_unread")["text"],
            format!("Held: {label}")
        );
        assert_eq!(
            snapshot.directory("act_held:personal:sig_unread")["hidden"],
            false
        );
    }
    let many: Vec<_> = (0..10)
        .map(|i| {
            let mut item = activity[0].clone();
            item["signal_id"] = json!(format!("row-{i}"));
            item
        })
        .collect();
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":many,"activity_filter":"all"}),
    );
    for _ in 0..9 {
        f.send(p, json!({"op":"test_directory_key","key":"down"}));
    }
    assert_eq!(inspect(&f, p)["directory_selection_visible"], true);
    control(&f, p, "reply_activity:row-9");
    let reply = inspect(&f, p);
    assert_eq!(reply["composer_open"], true);
    assert_eq!(reply["directory_selected_id"], "row-9");
    assert_eq!(reply["directory_selection_visible"], true);
}
#[test]
fn r1_passive_refresh_preserves_viewports_errors_and_pending_composer() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    frame["listeners"] = json!((0..12)
        .map(|i| {
            let mut row = listener();
            row["listener_id"] = json!(format!("row-{i}"));
            row["public_id"] = row["listener_id"].clone();
            row["scoped_id"] = json!(format!("personal:row-{i}"));
            row["session_id"] = row["listener_id"].clone();
            row["display_address"] = json!(format!("alice/fixture#{i:04x}"));
            row
        })
        .collect::<Vec<_>>());
    frame["my_remote_listeners"] = json!((0..12)
        .map(|i| peer(&format!("remote-{i}")))
        .collect::<Vec<_>>());
    frame["available"] = json!((0..6).map(|i|json!({"harness":"fixture","session_id":format!("candidate-{i}"),"title":"Review","cwd":"/tmp","endpoint_pid":4242})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    control(&f, p, "toggle_my_remote");
    control(&f, p, "toggle_available");
    // The available document starts with a 28pt group header. Scroll into its first candidate.
    let scrolls = [
        ("active_scroll", 20),
        ("my_remote_scroll", 20),
        ("available_scroll", 48),
    ];
    for (id, offset) in scrolls {
        f.send(p, json!({"op":"test_scroll","id":id,"offset":offset}));
    }
    let scrolled = inspect(&f, p);
    for (id, offset) in scrolls {
        assert_eq!(scrolled.menu(id)["content_y"], offset, "{id}");
    }
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    for (id, offset) in scrolls {
        assert_eq!(
            refreshed.menu(id)["content_y"],
            offset,
            "{id} must survive a rebuild"
        );
    }
    for (field, scroll_id, delta) in [
        (
            "listeners",
            "active_scroll",
            refreshed.menu("session:personal:row-0")["height"]
                .as_f64()
                .unwrap(),
        ),
        ("my_remote_listeners", "my_remote_scroll", 34.0),
    ] {
        let mut row = frame[field][0].clone();
        row["scoped_id"] = json!("personal:inserted");
        row["public_id"] = json!("inserted");
        row["session_id"] = json!("inserted");
        frame[field].as_array_mut().unwrap().insert(0, row);
        f.send(p, frame.clone());
        assert_eq!(
            inspect(&f, p).menu(scroll_id)["content_y"]
                .as_f64()
                .unwrap(),
            20.0 + delta,
            "{scroll_id} preserves its viewed row after insertion"
        );
        frame[field].as_array_mut().unwrap().remove(0);
        f.send(p, frame.clone());
        assert_eq!(inspect(&f, p).menu(scroll_id)["content_y"], 20);
    }
    // A second expanded harness shares the same viewport and does not move the anchor.
    let mut expanded = frame.clone();
    for i in 0..6 {
        expanded["available"].as_array_mut().unwrap().push(json!({"harness":"second","session_id":format!("other-{i}"),"title":"Review","cwd":"/tmp","endpoint_pid":4242}));
    }
    f.send(p, expanded);
    let snapshot = inspect(&f, p);
    let available_scroll = snapshot.menu("available_scroll");
    assert_eq!(available_scroll["height"], 200);
    assert_eq!(available_scroll["content_y"], 48);
    let available_rows = nodes(&available_scroll["children"]);
    for id in [
        "toggle_group:fixture",
        "candidate:fixture:candidate-0",
        "toggle_group:second",
        "candidate:second:other-0",
    ] {
        assert!(available_rows.iter().any(|node| node["id"] == id), "{id}");
    }
    assert_eq!(
        nodes(&snapshot["items"])
            .iter()
            .filter(|node| node["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("available_scroll")))
            .count(),
        1
    );
    control(&f, p, "toggle_group:second");
    let grouped = inspect(&f, p);
    assert!(grouped.menu("candidate:second:other-0").is_null());
    assert_eq!(grouped.menu("available_scroll")["content_y"], 48);
    control(&f, p, "toggle_available");
    assert!(inspect(&f, p).menu("available_scroll").is_null());
    control(&f, p, "toggle_available");
    let reopened = inspect(&f, p);
    assert_eq!(reopened.menu("available_scroll")["content_y"], 48);
    assert_eq!(
        reopened.menu("toggle_group:second")["symbol"],
        "chevron.right"
    );
    let mut inserted = frame["available"][0].clone();
    inserted["session_id"] = json!("inserted");
    frame["available"]
        .as_array_mut()
        .unwrap()
        .insert(0, inserted);
    f.send(p, frame.clone());
    assert_eq!(
        inspect(&f, p).menu("available_scroll")["content_y"],
        92,
        "Keep candidate-0 at the same screen position after an insertion"
    );
    frame["available"][0]["needs_reload"] = json!(true);
    f.send(p, frame.clone());
    assert_eq!(
        inspect(&f, p).menu("available_scroll")["content_y"],
        106,
        "Keep the anchor after a row above it grows"
    );
    frame["available"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(inspect(&f, p).menu("available_scroll")["content_y"], 48);
    control(&f, p, "open_directory");
    send_companion(
        &f,
        p,
        json!({"op":"error","source_op":"listen_session","error":"BRIDGE_UNAVAILABLE: Retry listening"}),
    );
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!inspect(&f, p).menu("menu_error_row").is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":["work: Hub temporarily unavailable"]}),
    );
    let warned = inspect(&f, p);
    assert_eq!(warned.directory("directory_warning_label")["hidden"], false);
    assert!(warned.directory("directory_warning_label")["tooltip"]
        .as_str()
        .unwrap()
        .contains("work"));
    assert_eq!(warned.directory("directory_scroll")["height"], 306);
    send_companion(
        &f,
        p,
        json!({"op":"error","source_op":"query_directory","error":"CONNECTION_FAILED: Remote Hub unavailable (os error 61)"}),
    );
    let failed = inspect(&f, p);
    assert!(failed.directory("directory_warning_label")["text"]
        .as_str()
        .unwrap()
        .starts_with("Cannot refresh"));
    assert!(
        !failed.directory("dir_peer:personal:one").is_null(),
        "A failed query must preserve usable rows"
    );
    assert!(!failed.menu("menu_error_row").is_null());
    let retry = failed.directory("directory_query_retry");
    assert_eq!(retry["accessibility_label"], "Retry search");
    assert_eq!(retry["hidden"], false);
    let previous = latest(&f, p, "query_directory");
    f.send(
        p,
        json!({"op":"test_control","id":"directory_query_retry","pointer":true}),
    );
    inspect(&f, p);
    let retried = latest(&f, p, "query_directory");
    assert_ne!(retried["request_id"], previous["request_id"]);
    for key in ["query", "scope", "harness"] {
        assert_eq!(retried[key], previous[key], "Retry changed {key}");
    }
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":[]}),
    );
    assert_eq!(
        inspect(&f, p).directory("directory_warning_label")["hidden"],
        true
    );
    control(&f, p, "open_settings");
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"Hub temporarily unavailable"}),
    );
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!inspect(&f, p).settings("settings_error_banner").is_null());
    control(&f, p, "dismiss_settings_error");
    assert!(inspect(&f, p).settings("settings_error_banner").is_null());
    control(&f, p, "close_settings");
    let peers = (0..12)
        .map(|i| peer(&format!("view-{i}")))
        .collect::<Vec<_>>();
    send_fixture(&f, p, json!({"op":"directory","peers":peers}));
    f.send(
        p,
        json!({"op":"test_scroll","id":"directory_scroll","offset":120}),
    );
    let mut more = peers.clone();
    more.insert(0, peer("inserted"));
    send_fixture(&f, p, json!({"op":"directory","peers":more}));
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        172,
        "Directory insertion preserves the viewed peer"
    );
    send_fixture(&f, p, json!({"op":"directory","peers":peers}));
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        120
    );
    send_fixture(&f, p, json!({"op":"directory","peers":[peer("one")]}));
    frame["activity"]=json!((0..12).map(|i|json!({"hub":"personal","signal_id":format!("sig-{i}"),"listener_id":"me","from_user":"bob","from_host_name":"laptop","from_title":"Review","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Review ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true})).collect::<Vec<_>>());
    f.send(p, frame.clone());
    control(&f, p, "open_activity");
    f.send(
        p,
        json!({"op":"test_scroll","id":"directory_scroll","offset":120}),
    );
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        120
    );
    frame["activity"][11]["message"] = json!("New details");
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["directory_selected_id"], "sig-0");
    assert_eq!(
        refreshed.directory("directory_scroll")["content_y"],
        120,
        "Passive refresh must not reveal an unchanged selection"
    );
    let mut inserted = frame["activity"][0].clone();
    inserted["signal_id"] = json!("inserted");
    frame["activity"]
        .as_array_mut()
        .unwrap()
        .insert(0, inserted);
    f.send(p, frame.clone());
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        180,
        "Insertion above the viewport preserves the viewed Activity row"
    );
    frame["activity"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        120,
        "Removing the inserted row restores the same anchor"
    );
    control(&f, p, "reply_activity:sig-0");
    assert_eq!(
        inspect(&f, p).directory("directory_scroll")["content_y"],
        0,
        "Opening the composer must reveal the selected row"
    );
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Review reply"}),
    );
    control(&f, p, "composer_send");
    control(&f, p, "composer_cancel");
    control(&f, p, "reply_activity:sig-0");
    assert!(
        nodes(&inspect(&f, p)["directory_items"])
            .iter()
            .any(|n| n["text"] == "Sending…"),
        "Reopening must retain pending-send status"
    );
    control(&f, p, "open_settings");
    let request = || {
        control(&f, p, "delete_activity:sig-0");
        inspect(&f, p);
        f.frames_for(p, "clear_activity_history")
            .next_back()
            .unwrap()["request_id"]
            .clone()
    };
    let stale = request();
    let current = request();
    assert_ne!(stale, current);
    let warning = || inspect(&f, p).directory("directory_warning_label");
    f.send(p, json!({"op":"error","source_op":"clear_activity_history","request_id":stale,"error":"Superseded"}));
    assert_eq!(warning()["hidden"], true);
    f.send(p, json!({"op":"state","ok":false,"source_op":"clear_activity_history","request_id":current,"error":"Current failure"}));
    assert_eq!(warning()["tooltip"], "Current failure");
    f.send(p, json!({"op":"directory","ok":true,"committed":true,"source_op":"clear_activity_history","request_id":current,"warning":"History cleared. Refresh will retry."}));
    assert_eq!(warning()["tooltip"], "History cleared. Refresh will retry.");
}

#[test]
fn in_flight_directory_query_filters_cached_rows_until_correlated_reply() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_directory");
    let mut mine = peer("mine");
    mine["is_self_principal"] = json!(true);
    let mut other_app = peer("other-app");
    other_app["is_self_principal"] = json!(true);
    other_app["harness"] = json!("custom");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("other-owner"), mine, other_app],"matched_count":7,"online_count":7}),
    );
    assert_eq!(inspect(&f, p)["directory_matched_count"], 7);
    control(&f, p, "dir_scope:mine");
    let scoped = inspect(&f, p);
    assert_eq!(scoped["directory_matched_count"], 2);
    assert!(scoped.directory("dir_row:personal:other-owner").is_null());
    control(&f, p, "dir_harness:fixture");
    let faceted = inspect(&f, p);
    assert_eq!(faceted["directory_matched_count"], 1);
    assert_eq!(faceted["directory_selected_id"], "personal:mine");
    f.send(p, json!({"op":"test_directory_query","query":"missing"}));
    let searching = inspect(&f, p);
    assert_eq!(searching["directory_matched_count"], 0);
    assert!(!searching.directory("directory_reset_filters").is_null());
    let mut result = peer("canonical");
    result["is_self_principal"] = json!(true);
    // The server can match fields omitted from the cached projection.
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[result],"matched_count":3,"online_count":7}),
    );
    let replied = inspect(&f, p);
    assert_eq!(replied["directory_matched_count"], 3);
    assert_eq!(replied["directory_selected_id"], "personal:canonical");
}

#[test]
fn directory_facets_distinguish_display_names_containing_separators() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    let mut first = frame["harnesses"][0].clone();
    first["display_name"] = json!("First|other:Second");
    frame["harnesses"] = json!([first]);
    f.send(p, frame.clone());
    control(&f, p, "open_directory");
    assert_eq!(
        inspect(&f, p).directory("dir_harness:fixture")["text"],
        "First|other:Second"
    );
    first["display_name"] = json!("First");
    let mut other = first.clone();
    other["id"] = json!("other");
    other["display_name"] = json!("Second");
    frame["harnesses"] = json!([first, other]);
    f.send(p, frame);
    let two = inspect(&f, p);
    assert_eq!(two.directory("dir_harness:fixture")["text"], "First");
    assert_eq!(two.directory("dir_harness:other")["text"], "Second");
}

#[test]
fn directory_app_pill_fits_or_truncates_and_reveals_its_full_frame() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let plugin_dir = f.home.join("fixture");
    fs::create_dir(&plugin_dir).unwrap();
    let mut frame = state();
    frame["listeners"] = json!([]);
    const TRACK: &str = "directory_harness_scroll";
    const APP: &str = "dir_harness:fixture";
    const ALL: &str = "dir_harness:all";
    let number = |view: &Value, key: &str| view[key].as_f64().unwrap();
    let right = |view: &Value| number(view, "x") + number(view, "width");
    for (bytes, title) in [
        (9, "Wide Work"),
        (
            59,
            "WIDE WEB WORKFLOW WORKSPACE WITH WORLDWIDE WORKER WORKBENCH",
        ),
    ] {
        assert_eq!(title.len(), bytes);
        fs::write(plugin_dir.join("plugin.json"), json!({"id":"fixture","kind":"agent-harness","displayName":title,"command":["./fixture"]}).to_string()).unwrap();
        let app = cito_native::plugins::parse_manifest_dir(&plugin_dir, false).unwrap();
        frame["harnesses"][0]["display_name"] = json!(app.display_name);
        f.send(p, frame.clone());
        if bytes == 9 {
            control(&f, p, "open_directory");
        }
        control(&f, p, ALL);
        let initial = inspect(&f, p);
        assert_eq!(initial.directory(TRACK)["selected_id"], ALL);
        assert_eq!(initial.directory(TRACK)["content_x"], 0);
        control(&f, p, APP);
        let selected = inspect(&f, p); // Also rejects unexpected truncation and clipping.
        let track = selected.directory(TRACK);
        let pill = selected.directory(APP);
        let all = selected.directory(ALL);
        let label = &pill["children"][0];
        let offset = number(&track, "content_x");
        let width = number(&track, "content_width");
        let doc_width = number(&track, "document_width");
        assert_eq!(selected["directory_harness"], "fixture");
        assert_eq!(track["selected_id"], APP);
        assert_eq!(pill["text"], title);
        assert_eq!(pill["tooltip"], title);
        assert_eq!(all["x"], 2);
        assert_eq!(number(&pill, "x"), right(&all) + 4.0);
        assert_eq!(right(&pill), doc_width - 2.0);
        assert!(number(&pill, "x") >= offset && right(&pill) <= offset + width);
        if bytes == 59 {
            assert!(doc_width > width && offset > 0.0);
            assert_eq!(number(&pill, "width"), width - 4.0);
            assert_eq!(right(&pill) + 2.0, offset + width);
            assert!(number(label, "label_measured_width") > number(label, "label_width"));
            assert_eq!(label["truncates_tail"], true);
        } else {
            assert_eq!(doc_width, width);
            assert_eq!(offset, 0.0);
            assert!(number(label, "label_measured_width") <= number(label, "label_width"));
        }
    }
}

#[test]
fn appkit_directory_keyboard_navigation_shortcuts_and_preserved_peers() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_directory");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":5,"unread_count":0,"activity":[]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["directory_open"], true);
    assert_eq!(s["directory_selected_id"], "personal:one");
    assert_eq!(s.directory("dir_scope:mine")["border_width"], 0);
    assert_eq!(s.directory("dir_harness:fixture")["border_width"], 0);
    assert_eq!(s.directory("dir_user:personal:one")["tooltip"], "@bob");
    assert_eq!(s.directory("dir_scope:all")["border_width"], 1);
    assert_eq!(s.directory("directory_panel")["width"], 560);
    for id in [
        "directory_scope_track",
        "directory_harness_scroll",
        "directory_mode_track",
    ] {
        assert!((s.directory(id)["background"][3].as_f64().unwrap() - 0.06).abs() < 0.001);
    }
    for membership in ["listeners", "hubs"] {
        let queries = f.frames_for(p, "query_directory").count();
        let mut changed = state();
        if membership == "listeners" {
            changed["listeners"] = json!([]);
        } else {
            changed["hubs"][0]["enabled"] = json!(false);
        }
        f.send(p, changed);
        inspect(&f, p);
        assert_eq!(
            f.frames_for(p, "query_directory").count(),
            queries + 1,
            "{membership}"
        );
        send_fixture(
            &f,
            p,
            json!({"op":"directory","peers":[peer("one"),peer("two")],"matched_count":2,"online_count":5}),
        );
        f.send(p, state());
        inspect(&f, p);
    }
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:two");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["clipboard"], peer("two")["display_address"]);
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(inspect(&f, p)["directory_selected_id"], "personal:one");
    control(&f, p, "msg_dir:personal:one");
    let before = f.frames(p).len();
    for oversized in ["x".repeat(65_537), "e\u{301}".repeat(32_769)] {
        f.send(p, json!({"op":"test_composer_input","value":oversized}));
        control(&f, p, "composer_send");
        let s = inspect(&f, p);
        assert_eq!(s.directory("composer_input")["text"], oversized);
        assert_eq!(
            s.directory("composer_error")["text"],
            "Message exceeds maximum length"
        );
        assert_eq!(
            f.frames(p).len(),
            before,
            "Oversized input must not be emitted"
        );
    }
    // Word breaks keep native text layout bounded while testing 65,536 Unicode scalars.
    let maximum = "😀 ".repeat(32_767) + "😀x";
    f.send(p, json!({"op":"test_composer_input","value":maximum}));
    control(&f, p, "composer_send");
    inspect(&f, p);
    let submitted = f.frames(p)[before..]
        .iter()
        .find(|v| v["op"] == "send_signal")
        .unwrap()
        .clone();
    assert_eq!(
        submitted["message"], maximum,
        "Local rejection must not leave the composer pending"
    );
    assert!(serde_json::to_vec(&submitted).unwrap().len() <= 1_048_576);
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":submitted["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Draft preserved"}),
    );
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
    });
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"OIDC cancelled"}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s.directory("composer_send")["text"], "Sending…");
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        1,
        "Unrelated errors must not permit a duplicate send"
    );
    let submitted = f.frames_for(p, "send_signal").next_back().unwrap();
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":submitted["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], false);
    assert_eq!(s.directory("composer_input")["text"], "");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Draft preserved"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        2,
        "The composer result must settle the send and allow a new one"
    );
    let failed = f.frames_for(p, "send_signal").next_back().unwrap();
    assert!(!failed["request_id"].as_str().unwrap().is_empty());
    f.send(
        p,
        json!({"op":"composer_result","ok":false,"error":"Hub unavailable. Retry.","request_id":failed["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s.directory("composer_input")["text"], "Draft preserved");
    assert_eq!(
        s.directory("composer_error")["text"],
        "Hub unavailable. Retry."
    );
    // A's trailing generic error can arrive after B has already been submitted.
    control(&f, p, "composer_send");
    let retry = latest(&f, p, "send_signal");
    assert_ne!(retry["request_id"], failed["request_id"]);
    f.send(p, json!({"op":"error","source_op":"send_signal","request_id":failed["request_id"],"error":"Late send error"}));
    f.send(p, json!({"op":"composer_result","ok":false,"request_id":failed["request_id"],"error":"Late result"}));
    for frame in [
        json!({"op":"composer_result","ok":true}),
        json!({"op":"composer_result","ok":false,"error":"Untagged result"}),
        json!({"op":"error","error":"Message is too large to send"}),
        json!({"op":"error","source_op":"send_signal","error":"Untagged send error"}),
    ] {
        f.send(p, frame);
    }
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s.directory("composer_send")["text"], "Sending…");
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Draft preserved")
            .count(),
        3,
        "A's late error must not permit a duplicate of B"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":retry["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);
    control(&f, p, "msg_dir:personal:one");
    f.send(p, json!({"op":"test_composer_input","value":"Next draft"}));
    f.send(p, json!({"op":"error","source_op":"send_signal","request_id":retry["request_id"],"error":"Already settled"}));
    let s = inspect(&f, p);
    assert_eq!(s.directory("composer_input")["text"], "Next draft");
    assert_eq!(s.directory("composer_error")["text"], "");
    control(&f, p, "composer_cancel");
    assert_eq!(
        inspect(&f, p).directory("directory_shortcuts")["text"],
        "↵ Copy Address   ⇧↵ Message   ⎋ Close"
    );
    control(&f, p, "dir_mode:activity");
    let activity: Vec<_> = ["reply_a", "reply_b"]
        .into_iter()
        .map(|id| {
            json!({"hub":"personal","signal_id":format!("personal:{id}"),
                "listener_id":"c_human","from_user":"bob","from_host_name":"laptop",
                "from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one",
                "target_harness":"human","target_title":"Me","target_scoped_id":"personal:me",
                "message":"Approve?","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true})
        })
        .collect();
    let mut frame = state();
    frame["activity"] = json!(activity);
    frame["unread_count"] = json!(2);
    f.send(p, frame);
    control(&f, p, "reply_activity:personal:reply_a");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Same reply text"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["to"], "personal:one");
    assert_eq!(replies[0]["hub"], "personal");
    assert_eq!(replies[0]["in_reply_to"], "personal:personal:reply_a");
    control(&f, p, "reply_activity:personal:reply_b");
    let s = inspect(&f, p);
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(s.directory("composer_input")["text"], "Same reply text");
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"error":null,"warning":"Message sent. Activity could not refresh.","request_id":replies[0]["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(s.directory("composer_input")["text"], "Same reply text");
    assert_eq!(s.directory("composer_send")["text"], "✓ Sent");
    let warning = s.directory("directory_warning_label");
    assert_eq!(warning["hidden"], false);
    assert_eq!(
        warning["tooltip"],
        "Message sent. Activity could not refresh."
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["to"], "personal:one");
    assert_eq!(replies[1]["hub"], "personal");
    assert_eq!(replies[1]["in_reply_to"], "personal:personal:reply_b");
    // Cancel only hides the draft; its in-flight send must still own its snapshot.
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:directory");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Same reply text"}),
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
            .count(),
        2,
        "Cancel must not allow a second send before the first result"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[1]["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s["composer_reply_signal_id"], "");
    assert_eq!(s.directory("composer_input")["text"], "Same reply text");
    control(&f, p, "composer_send");
    inspect(&f, p);
    let replies: Vec<_> = f
        .frames(p)
        .into_iter()
        .filter(|v| v["op"] == "send_signal" && v["message"] == "Same reply text")
        .collect();
    assert_eq!(replies.len(), 3);
    assert!(replies[2]["in_reply_to"].is_null());
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[2]["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);

    // Recreating the same visible draft gives it a different send identity.
    let submit_again = || {
        control(&f, p, "msg_dir:personal:one");
        f.send(
            p,
            json!({"op":"test_composer_input","value":"Identical draft"}),
        );
        control(&f, p, "composer_send");
        inspect(&f, p);
        f.frames_for(p, "send_signal").next_back().unwrap()
    };
    let prior = submit_again();
    control(&f, p, "composer_cancel");
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Identical draft"}),
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":prior["request_id"]}),
    );
    let reopened = inspect(&f, p);
    assert_eq!(reopened["composer_open"], true);
    assert_eq!(
        reopened.directory("composer_input")["text"],
        prior["message"]
    );
    control(&f, p, "composer_send");
    inspect(&f, p);
    let current = f.frames_for(p, "send_signal").next_back().unwrap();
    assert_eq!(current["to"], prior["to"]);
    assert_eq!(current["message"], prior["message"]);
    assert_ne!(current["idempotency_key"], prior["idempotency_key"]);
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":current["request_id"]}),
    );
    assert_eq!(inspect(&f, p)["composer_open"], false);

    f.send(p, state());
    assert!(!inspect(&f, p).directory("msg_dir:personal:one").is_null());
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);
}
#[test]
fn composer_settles_empty_errors_and_surfaces_late_failures_after_close() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_directory");
    send_fixture(&f, p, json!({"op":"directory","peers":[peer("one")]}));
    let submit = |draft: &str| {
        control(&f, p, "msg_dir:personal:one");
        f.send(p, json!({"op":"test_composer_input","value":draft}));
        control(&f, p, "composer_send");
        let submitted = inspect(&f, p);
        assert_eq!(submitted.directory("composer_send")["text"], "Sending…");
        f.frames(p)
            .into_iter()
            .find(|v| v["op"] == "send_signal" && v["message"] == draft)
            .unwrap()["request_id"]
            .clone()
    };
    let empty_error = submit("First draft");
    f.send(
        p,
        json!({"op":"error","source_op":"send_signal","error":"","request_id":empty_error}),
    );
    let settled = inspect(&f, p);
    assert_eq!(settled.directory("composer_send")["enabled"], true);
    assert_eq!(settled.directory("composer_input")["text"], "First draft");
    assert_eq!(settled.directory("directory_warning_label")["hidden"], true);

    for (draft, mut receipt) in [
        (
            "Second draft",
            json!({"op":"composer_result","ok":false,"error":"The Hub cannot deliver this message."}),
        ),
        (
            "Third draft",
            json!({"op":"error","source_op":"send_signal","error":"The connection closed during send."}),
        ),
    ] {
        receipt["request_id"] = submit(draft);
        control(&f, p, "composer_cancel");
        let message = receipt["error"].clone();
        f.send(p, receipt);
        let closed = inspect(&f, p);
        assert_eq!(closed["composer_open"], false);
        let warning = closed.directory("directory_warning_label");
        assert_eq!(warning["hidden"], false);
        assert_eq!(warning["tooltip"], message);
    }
    let success = submit("Fourth draft");
    control(&f, p, "composer_cancel");
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"error":null,"warning":"","request_id":success}),
    );
    let final_state = inspect(&f, p);
    assert_eq!(
        final_state.directory("directory_warning_label")["tooltip"],
        "The connection closed during send."
    );
}

#[test]
fn v29_full_message_copy_is_correlated_and_never_copies_a_preview() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let mut l = ClosedLoop::new(&f);
    l.until(|s| s.menu("header").is_object());
    let listening = f.tool("cito_listen", json!({"about":"Copy fixture"}), meta.clone());
    assert_eq!(listening["ok"], true, "{listening}");
    l.until(|s| {
        nodes(&s["items"])
            .iter()
            .any(|n| n["id"].as_str().is_some_and(|id| id.starts_with("copy:")))
    });
    let message = "😀 ".repeat(32_768);
    let sent = f.tool("cito_send", json!({"to":"@self","message":message}), meta);
    assert_eq!(sent["ok"], true, "{sent}");
    wait(|| !f.rows("SELECT signal_id FROM local_signals").is_empty());
    l.control("open_activity");
    let s = l.until(|s| {
        nodes(&s["directory_items"]).iter().any(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("copy_activity_msg:"))
        })
    });
    let control_id = nodes(&s["directory_items"])
        .into_iter()
        .find_map(|n| {
            n["id"]
                .as_str()
                .filter(|id| id.starts_with("copy_activity_msg:"))
                .map(str::to_owned)
        })
        .unwrap();
    f.send(l.ui, json!({"op":"test_clipboard","text":"old clipboard"}));
    l.control(&control_id);
    assert_eq!(inspect(&f, l.ui)["clipboard"], "old clipboard");
    let copied = l.until(|s| s["clipboard"] == message.trim());
    assert_eq!(copied["clipboard"], message.trim());
    assert_eq!(copied.directory(&control_id)["text"], "✓ Copied");
    l.control(&control_id);
    wait(|| f.frames_for(l.ui, "copy_activity_message").count() == 2);
    let second = f
        .frames_for(l.ui, "copy_activity_message")
        .next_back()
        .unwrap();
    wait(|| {
        l.pump();
        l.frames.iter().any(|frame| {
            frame["op"] == "activity_message" && frame["request_id"] == second["request_id"]
        })
    });
    f.send(
        l.ui,
        json!({"op":"test_expire_copy_feedback","id":control_id}),
    );
    let restored = inspect(&f, l.ui);
    let button = restored.directory(&control_id);
    assert_eq!(button["text"], "Copy");
    assert_eq!(button["accessibility_label"], "Copy message");
    let frames: Vec<_> = l
        .frames
        .iter()
        .filter(|f| f["op"] == "activity_message")
        .collect();
    let requests: Vec<_> = f.frames_for(l.ui, "copy_activity_message").collect();
    assert_eq!(frames.len(), requests.len());
    for (frame, request) in frames.into_iter().zip(requests) {
        assert_eq!(
            frame,
            &json!({"op":"activity_message","request_id":request["request_id"],"message":message.trim()})
        );
    }
    l.control(&control_id);
    let address = nodes(&restored["items"])
        .into_iter()
        .find(|n| n["id"].as_str().is_some_and(|id| id.starts_with("copy:")))
        .unwrap();
    let expected = address["text"].clone();
    l.control(address["id"].as_str().unwrap());
    l.pump();
    assert_eq!(
        inspect(&f, l.ui)["clipboard"],
        expected,
        "A newer clipboard action supersedes an in-flight copy receipt"
    );
    assert_eq!(inspect(&f, l.ui)["composer_open"], false);
    let copy_count = f.frames_for(l.ui, "copy_activity_message").count();
    l.control(&control_id);
    wait(|| f.frames_for(l.ui, "copy_activity_message").count() == copy_count + 1);
    let request = f
        .frames_for(l.ui, "copy_activity_message")
        .next_back()
        .unwrap();
    l.control("dir_mode:directory");
    f.send(l.ui,json!({"op":"activity_message","request_id":request["request_id"],"message":message.trim()}));
    let switched = inspect(&f, l.ui);
    assert_eq!(
        switched["clipboard"],
        message.trim(),
        "A hidden Activity row still completes its copy"
    );
    let addresses: Vec<_> = nodes(&switched["directory_items"])
        .into_iter()
        .filter(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("copy_dir:"))
        })
        .collect();
    assert!(!addresses.is_empty());
    assert!(
        addresses.iter().all(|n| n["text"] != "✓ Copied"),
        "An Activity completion must not decorate reused Directory cells"
    );
    l.control(address["id"].as_str().unwrap());
    l.control("dir_mode:activity");
    l.until(|s| !s.directory(&control_id).is_null());
    for invalid_receipt in [false, true] {
        l.control(&control_id);
        let request = f
            .frames_for(l.ui, "copy_activity_message")
            .next_back()
            .unwrap();
        send_companion(
            &f,
            l.ui,
            if invalid_receipt {
                json!({"op":"activity_message","request_id":request["request_id"],"error":"COPY_FAILED: Message unavailable"})
            } else {
                json!({"op":"error","source_op":"copy_activity_message","request_id":request["request_id"],"error":"COPY_FAILED: Message unavailable"})
            },
        );
        let failed = inspect(&f, l.ui);
        assert_eq!(failed["clipboard"], expected);
        let status = failed.directory("directory_summary");
        assert_eq!(status["text"], "Message unavailable");
        assert_eq!(status["tooltip"], "Message unavailable");
        assert_eq!(status["hidden"], false);
        l.pump();
        assert_eq!(inspect(&f, l.ui)["clipboard"], expected);
        let query = f
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|action| action["op"] == "query_directory" || action["op"] == "open_directory")
            .unwrap();
        f.send(l.ui,json!({"op":"directory","request_id":query["request_id"],"query":query["query"],"scope":query["scope"],"harness":query["harness"],"peers":[],"warnings":[]}));
        let warning = inspect(&f, l.ui).directory("directory_warning_label");
        assert_eq!(warning["hidden"], false);
        assert_eq!(warning["tooltip"], "Message unavailable");
        l.control(&control_id);
        let success = l.until(|s| s["clipboard"] == message.trim());
        assert_eq!(
            success.directory("directory_warning_label")["hidden"],
            true,
            "Only the successful correlated retry settles the copy error"
        );
        assert_eq!(success.directory("directory_summary")["tooltip"], "");
        l.control(address["id"].as_str().unwrap());
        assert_eq!(inspect(&f, l.ui)["clipboard"], expected);
    }
    // Add a hosted invite and reload control to exercise clipboard arbitration.
    let mut state = super::state();
    state["listeners"] = json!([]);
    state["settings"] = f.snapshot()["settings"].clone();
    state["settings"]["host_hub"]["enabled"] = json!(true);
    state["settings"]["host_hub"]["running"] = json!(true);
    state["settings"]["host_hub"]["token"] = json!("fixture-token");
    state["settings"]["host_hub"]["auth_mode"] = json!("token");
    state["settings"]["host_hub"]["invite_command"] =
        json!("cito setup --hub work --url http://127.0.0.1:4291");
    state["settings"]["host_hub"]["lan_url"] = Value::Null;
    state["settings"]["host_hub"]["tailscale_url"] = Value::Null;
    state["settings"]["host_hub"]["lan_invite_command"] = Value::Null;
    state["available"] = json!([{"harness":"fixture","session_id":"reload","title":"Reload bridge","cwd":"/tmp","needs_reload":true}]);
    f.send(l.ui, state);
    l.control("toggle_available");
    l.control("open_settings");
    l.control("settings_tab:hubs");
    for (id, expected) in [
        (
            "copy_hub_invite",
            "cito setup --hub work --url http://127.0.0.1:4291",
        ),
        ("reload:fixture:reload", "/reload"),
    ] {
        l.control(&control_id);
        let request = f
            .frames_for(l.ui, "copy_activity_message")
            .next_back()
            .unwrap();
        l.control(id);
        f.send(l.ui, json!({"op":"activity_message","request_id":request["request_id"],"message":"late full message"}));
        let copied = inspect(&f, l.ui);
        assert_eq!(copied["clipboard"], expected, "{id}");
        if id == "copy_hub_invite" {
            assert_eq!(copied.settings(id)["text"], "✓ Invite Copied");
        }
    }
    // Real production actions must emit non-coalesced success receipts, not just passive snapshots.
    l.pump();
    for op in ["ack_signals", "clear_activity_history"] {
        let request = format!("round4-{op}");
        let (status, result) = f.action(json!({"op":op,"request_id":request,"ack_all":true}));
        assert_eq!(status, 200, "{result}");
        wait(|| {
            l.pump();
            l.frames.iter().any(|frame| {
                frame["op"] == "state"
                    && frame["ok"] == true
                    && frame["request_id"] == request
                    && frame["source_op"] == op
            })
        });
        assert_eq!(
            inspect(&f, l.ui).directory("directory_warning_label")["hidden"],
            true
        );
    }
    assert!(f
        .rows("SELECT signal_id FROM local_signals WHERE activity_hidden=0")
        .is_empty());
    assert!(!f
        .rows("SELECT signal_id FROM local_signals WHERE activity_hidden=1")
        .is_empty());
}

#[test]
fn activity_selector_disambiguates_the_same_opaque_id_on_two_hubs() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    f.send(p, state());
    control(&f, p, "open_activity");
    control(&f, p, "act_filter:all");
    let activity = ["personal", "work"].map(|hub| json!({"hub":hub,"signal_id":"thread:one","listener_id":"recipient","from_user":"bob","from_host_name":"desktop","from_title":"Build","target_harness":"human","target_title":"You","message":"Build complete","created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true}));
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":activity,"activity_filter":"all"}),
    );
    assert_eq!(
        inspect(&f, p)["directory_selected_activity"],
        json!({"hub":"personal","signal_id":"thread:one"})
    );
    control(&f, p, "delete_activity:thread:one");
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "clear_activity_history").count(), 0);
    f.send(p, json!({"op":"test_control","id":"activity_row:thread:one","hub":"work","signal_id":"thread:one"}));
    assert_eq!(
        inspect(&f, p)["directory_selected_activity"],
        json!({"hub":"work","signal_id":"thread:one"})
    );
    f.send(p, json!({"op":"test_control","id":"delete_activity:thread:one","hub":"work","signal_id":"thread:one"}));
    let action = latest(&f, p, "clear_activity_history");
    assert_eq!(action["hub"], "work");
    assert_eq!(action["signal_id"], "thread:one");
}

#[test]
fn directory_2a_composer_binding_validation_delete_guards_facets_and_attachments() {
    let _lock = serial();
    let (f, p) = ui_fixture();
    let mut frame = state();
    let mut extra_harness = frame["harnesses"][0].clone();
    extra_harness["id"] = json!("external-app");
    extra_harness["display_name"] = json!("External App");
    frame["harnesses"] = json!([frame["harnesses"][0].clone(), extra_harness]);
    f.send(p, frame.clone());
    control(&f, p, "open_directory");

    // 1. Address chip copy-action AX label and DISCONNECTED_RETAINED attachment filtering.
    let mut human_one = peer("alice-one");
    human_one["harness"] = json!("human");
    human_one["user"] = json!("alice");
    human_one["display_address"] = json!("@alice");
    human_one["attachments"] = json!([
        {"channel_id":"dev:active","location":"device","host_id":"h1","display_name":"laptop","availability":"ACTIVE","is_local":false},
        {"channel_id":"dev:stale","location":"device","host_id":"h2","display_name":"desktop","availability":"DISCONNECTED_RETAINED","is_local":true}
    ]);
    let mut human_two = peer("alice-two");
    human_two["scoped_id"] = json!("personal:alice-two");
    human_two["harness"] = json!("human");
    human_two["user"] = json!("alice");
    human_two["display_address"] = json!("@alice");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[human_one, human_two],"matched_count":2,"online_count":2}),
    );
    let initial = inspect(&f, p);
    assert_eq!(
        initial.directory("copy_dir:personal:alice-one")["accessibility_label"],
        "Copy address @alice"
    );
    assert_eq!(
        initial.directory("dir_meta:personal:alice-one")["text"],
        "Inbox · laptop · Schema work"
    );

    // 2. Removed external App facet resets to "all".
    control(&f, p, "dir_harness:external-app");
    assert_eq!(inspect(&f, p)["directory_harness"], "external-app");
    frame["harnesses"] = json!([frame["harnesses"][0].clone()]);
    f.send(p, frame.clone());
    let reset_facet = inspect(&f, p);
    assert_eq!(reset_facet["directory_harness"], "all");
    assert_eq!(reset_facet.directory("dir_harness:all")["border_width"], 1);

    // 3. Composer binds @alice target to stable scoped_id, shares blank validation,
    // and ignores Search Return while Search is focused.
    control(&f, p, "msg_dir:personal:alice-two");
    let opened = inspect(&f, p);
    assert_eq!(opened["composer_target"], "personal:alice-two");
    assert_eq!(opened.directory("composer_target")["text"], "To: @alice");

    f.send(p, json!({"op":"test_composer_input","value":"   "}));
    control(&f, p, "composer_send");
    let blank_send = inspect(&f, p);
    assert_eq!(
        blank_send.directory("composer_error")["text"],
        "Enter a message to send."
    );
    assert_eq!(f.frames_for(p, "send_signal").count(), 0);

    f.send(
        p,
        json!({"op":"test_composer_input","value":"Ready to send"}),
    );
    f.send(p, json!({"op":"test_focus_field","id":"directory_search"}));
    f.send(
        p,
        json!({"op":"test_directory_key","key":"return","via":"field"}),
    );
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "send_signal").count(), 0);

    f.send(p, json!({"op":"test_focus_field","id":"composer_input"}));
    f.send(
        p,
        json!({"op":"test_directory_key","key":"return","via":"field"}),
    );
    let sent = latest(&f, p, "send_signal");
    assert_eq!(sent["to"], "personal:alice-two");
    assert_eq!(sent["hub"], "personal");
    assert_eq!(sent["message"], "Ready to send");
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":sent["request_id"]}),
    );

    // 4. Reply binds to from_scoped_id and scopes colon-bearing signal_id for in_reply_to.
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    let item = |id: &str, msg: &str| json!({"hub":"work","signal_id":id,"listener_id":"c_human","from_user":"alice","from_host_name":"laptop","from_harness":"human","from_title":"Alice","from_scoped_id":"work:c_alice","from_display_address":"@alice","target_harness":"human","target_title":"You","target_scoped_id":"work:me","message":msg,"created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":true,"can_reply":true});
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[item("thread:source", "Hello"), item("sibling", "Other")],"activity_filter":"all","activity_total":2}),
    );
    control(&f, p, "reply_activity:thread:source");
    f.send(p, json!({"op":"test_composer_input","value":"Reply body"}));
    control(&f, p, "composer_send");
    let reply = latest(&f, p, "send_signal");
    assert_eq!(reply["to"], "work:c_alice");
    assert_eq!(reply["hub"], "work");
    assert_eq!(reply["in_reply_to"], "work:thread:source");
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":reply["request_id"]}),
    );

    // 5. Block row × Delete during Clear N… confirm, and preserve unchanged row press
    // across passive sibling updates.
    control(&f, p, "activity_clear");
    assert_eq!(
        inspect(&f, p).directory("delete_activity:thread:source")["enabled"],
        false
    );
    control(&f, p, "delete_activity:thread:source");
    inspect(&f, p);
    assert_eq!(f.frames_for(p, "clear_activity_history").count(), 0);
    control(&f, p, "activity_clear_confirm_cancel");
    assert_eq!(
        inspect(&f, p).directory("delete_activity:thread:source")["enabled"],
        true
    );

    let request = latest(&f, p, "query_directory");
    let mut sibling_update = json!({"op":"directory","peers":[],"activity":[item("thread:source", "Hello"), item("sibling", "Updated sibling")],"activity_filter":"all","activity_total":2});
    for key in ["request_id", "query", "scope", "harness"] {
        sibling_update[key] = request[key].clone();
    }
    f.send(
        p,
        json!({"op":"test_tracked_row_press","id":"delete_activity:thread:source","refresh":sibling_update}),
    );
    wait(|| f.frames_for(p, "clear_activity_history").count() == 1);
    assert_eq!(
        latest(&f, p, "clear_activity_history")["signal_id"],
        "thread:source"
    );
}
