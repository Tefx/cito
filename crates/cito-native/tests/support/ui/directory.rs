use super::*;

#[test]
fn v41_directory_copy_drafts_and_activity_confirmation_survive_only_valid_transitions() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
        node(&inspect(&f, p), "directory_items", "copy_dir:personal:one")["text"],
        "✓ Copied"
    );
    control(&f, p, "msg_dir:personal:one");
    f.send(
        p,
        json!({"op":"test_composer_input","value":"Keep this draft"}),
    );
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    f.send(p, json!({"op":"test_directory_key","key":"shift-return"}));
    let draft = inspect(&f, p);
    assert_eq!(draft["composer_target"], peer("one")["display_address"]);
    assert_eq!(
        node(&draft, "directory_items", "composer_input")["text"],
        "Keep this draft"
    );
    control(&f, p, "composer_cancel");
    control(&f, p, "dir_mode:activity");
    control(&f, p, "act_filter:all");
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[],"activity":[{"hub":"personal","signal_id":"observed","listener_id":"agent","from_user":"bob","from_host_name":"desktop","from_title":"Build","target_harness":"fixture","target_title":"Session","target_scoped_id":"personal:agent","message":"Build ready","created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":false}]}),
    );
    control(&f, p, "activity_clear");
    assert!(!node(
        &inspect(&f, p),
        "directory_items",
        "activity_clear_confirm_commit"
    )
    .is_null());
    f.send(p, json!({"op":"test_directory_query","query":"Build"}));
    assert!(node(
        &inspect(&f, p),
        "directory_items",
        "activity_clear_confirm_commit"
    )
    .is_null());
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    inspect(&f, p);
    assert!(!f.frames(p).iter().any(|a| a["op"] == "ack_signals"));
}

#[test]
fn inbox_scope_gating_read_all_search_and_confirmed_empty_directory() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["settings"] = settings();
    frame["hubs"][0]["human_listener_id"] = json!("inbox");
    frame["hubs"][0]["human_state_status"] = json!("current");
    frame["hubs"][0]["human_scope"] = json!("workspace");
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
    let compact = node(&inspect(&f, p), "items", &copy_id)["text"].clone();
    assert_ne!(compact, full_address);
    control(&f, p, &copy_id);
    inspect(&f, p);
    f.send(p, json!({"op":"test_expire_copy_feedback","id":copy_id}));
    let restored = inspect(&f, p);
    assert_eq!(node(&restored, "items", &copy_id)["text"], compact);
    assert_eq!(
        node(&restored, "items", &copy_id)["accessibility_label"],
        full_address
    );
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
        let inbox = node(&s, "settings_items", "scope:inbox");
        let agent = node(&s, "items", &format!("scope:{agent_id}"));
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
    inspect(&f, p);
    control(&f, p, "activity_mark_all_read");
    let read = inspect(&f, p);
    assert_eq!(read["unread_count"], 1);
    assert!(f
        .frames(p)
        .iter()
        .any(|a| a["op"] == "ack_signals" && a["ack_all"] == true && a["query"] == "Review ready"));
    f.send(
        p,
        json!({"op":"test_directory_query","query":"Build passed"}),
    );
    let remaining = inspect(&f, p);
    assert_eq!(
        node(&remaining, "directory_items", "activity_mark_all_read")["enabled"],
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
    assert!(node(&paused, "settings_items", "inbox_control_bar").is_object());
    assert_eq!(
        node(&paused, "settings_items", "toggle_inbox")["enabled"],
        false
    );
    control(&f, p, "open_uninstall_confirm");
    control(&f, p, "uninstall_purge_data");
    let purge = inspect(&f, p);
    assert!(
        node(&purge, "settings_items", "confirm_disconnect_all")["tooltip"]
            .as_str()
            .unwrap()
            .contains("delete local data and Hub settings")
    );
}

#[test]
fn v39_reload_hint_retains_full_tooltip_and_activity_reset_requeries() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    frame["harnesses"][0]["display_name"] = json!("Claude Code");
    frame["available"] = json!([{"harness":"fixture","session_id":"reload","title":"Review","cwd":"/tmp","needs_reload":true}]);
    frame["activity_has_any"] = json!(true);
    f.send(p, frame);
    let hint = node(&inspect(&f, p), "items", "candidate_hint:fixture:reload");
    assert_eq!(
        hint["tooltip"],
        "Run /reload in Claude Code to enable 1-click listen"
    );
    assert_eq!(hint["text"], hint["tooltip"]);
    assert_eq!(hint["width"], 244);
    assert_eq!(hint["x"], 46);
    control(&f, p, "open_activity");
    f.send(p, json!({"op":"test_directory_query","query":"missing"}));
    assert!(!node(&inspect(&f, p), "directory_items", "act_reset_search").is_null());
    control(&f, p, "act_reset_search");
    let s = inspect(&f, p);
    assert_eq!(node(&s, "directory_items", "directory_search")["text"], "");
    let query = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "query_directory")
        .unwrap();
    assert_eq!(query["query"], "");
    assert_eq!(query["mode"], "activity");
    control(&f, p, "act_filter:me");
    assert!(!node(&inspect(&f, p), "directory_items", "act_empty_show_all").is_null());
    control(&f, p, "act_empty_show_all");
    let s = inspect(&f, p);
    assert_eq!(s["activity_filter"], "all");
    let query = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "query_directory")
        .unwrap();
    assert_eq!(query["filter"], "all");
}

#[test]
fn appkit_directory_human_labels_normalize_legacy_defaults_and_preserve_custom_details() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    f.send(p, state());
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
            node(&s, "directory_items", "dir_title:personal:human")["text"],
            expected_title
        );
        assert_eq!(
            node(&s, "directory_items", "dir_meta:personal:human")["text"],
            if expected_about.is_empty() {
                "Inbox · laptop".into()
            } else {
                format!("Inbox · laptop · {expected_about}")
            }
        );
        assert_eq!(
            node(&s, "directory_items", "dir_title:personal:agent")["text"],
            "Fixture"
        );
        assert_eq!(
            node(&s, "directory_items", "dir_meta:personal:agent")["text"],
            " · laptop"
        );
    }
    let mut frame = state();
    frame["hubs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"local","status":"connected","auth_mode":"none","user":"bob"}));
    f.send(p, frame.clone());
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
    f.send(p, frame);
    let remote = node(&inspect(&f, p), "items", "remote:personal:home");
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
        node(&s, "directory_items", "dir_title:personal:home")["text"],
        "Release audit"
    );
    assert_eq!(
        node(&s, "directory_items", "dir_meta:personal:home")["text"],
        " · laptop, mac-studio"
    );
    assert_eq!(
        node(&s, "directory_items", "dir_repo:personal:home")["text"],
        " · ~"
    );
    assert!(!node(&s, "directory_items", "dir_title:local:home").is_null());
    assert_eq!(s["directory_matched_count"], 2);
    assert_eq!(s["directory_online_count"], 2);
    agent["about"] = json!("");
    send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
    assert_eq!(
        node(
            &inspect(&f, p),
            "directory_items",
            "dir_title:personal:home"
        )["text"],
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
                node(
                    &inspect(&f, p),
                    "directory_items",
                    "dir_title:personal:home"
                )["text"],
                expected,
                "raw={raw}, about={about}"
            );
            if about == description_home || about == at_description_home {
                assert_eq!(
                    node(&inspect(&f, p), "directory_items", "dir_meta:personal:home")["tooltip"],
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
            node(
                &inspect(&f, p),
                "directory_items",
                "dir_title:personal:home"
            )["text"],
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
                node(&s, "directory_items", "dir_title:personal:home")["text"],
                "Auth refactor"
            );
            assert!(
                !node(&s, "directory_items", "dir_meta:personal:home")["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("Auth refactor")
            );
        }
    }
    agent["custom_title"] = json!(true);
    for title in ["bob", "Human"] {
        agent["title"] = json!(title);
        send_fixture(&f, p, json!({"op":"directory","peers":[agent.clone()]}));
        assert_eq!(
            node(
                &inspect(&f, p),
                "directory_items",
                "dir_title:personal:home"
            )["text"],
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
        f.send(p, frame);
        assert_eq!(
            node(&inspect(&f, p), "directory_items", "act_title:legacy-human")["text"],
            expected
        );
    }
}

#[test]
fn appkit_activity_reply_filters_navigation_and_read_all() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
    let mut frame = state();
    let activity = json!([{"hub":"personal","signal_id":"personal:sig_unread","listener_id":"c_human","from_user":"bob","from_host_name":"laptop","from_harness":"fixture","from_title":"Schema work","from_scoped_id":"personal:one","target_harness":"human","target_title":"Me","target_scoped_id":"personal:me","message":"Approve?","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":true,"for_me":true,"can_reply":true},{"hub":"personal","signal_id":"personal:sig_read","listener_id":"c_other","from_user":"alice","from_host_name":"desktop","from_harness":"fixture","from_title":"Other","from_scoped_id":"personal:two","target_harness":"fixture","target_title":"Other","target_scoped_id":"personal:other","message":"Done","held_reason":null,"in_reply_to":null,"created_at":"2030-01-01T00:00:00Z","unread":false,"for_me":false}]);
    frame["activity"] = activity.clone();
    frame["unread_count"] = json!(1);
    f.send(p, frame);
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"matched_count":1,"online_count":2,"activity":activity,"unread_count":1}),
    );
    control(&f, p, "open_activity");
    let s = inspect(&f, p);
    assert_eq!(s["directory_mode"], "activity");
    assert!(
        (node(&s, "directory_items", "activity_filter_track")["background"][3]
            .as_f64()
            .unwrap()
            - 0.06)
            .abs()
            < 0.001
    );
    let collapsed = node(&s, "directory_items", "act_msg:personal:sig_unread");
    assert_eq!(
        collapsed["y"].as_f64().unwrap() + collapsed["height"].as_f64().unwrap(),
        26.0
    );
    assert_eq!(
        node(&s, "directory_items", "act_filter:unread")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "act_filter:me")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "activity_mark_all_read")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "activity_clear")["border_width"],
        1
    );
    assert_eq!(
        node(&s, "directory_items", "directory_shortcuts")["text"],
        "↵ Reply    ⎋ Close"
    );
    let row = node(&s, "directory_items", "activity_row:personal:sig_unread");
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
        node(&s, "directory_items", "act_time:personal:sig_unread")["text"],
        "Just now"
    );
    assert_eq!(
        node(&s, "directory_items", "act_msg:personal:sig_unread")["width"],
        492
    );
    let delete = node(&s, "directory_items", "delete_activity:personal:sig_unread");
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
    let deletions = f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "clear_activity_history")
        .count();
    f.send(p, json!({"op":"test_focus_field","id":"directory_search"}));
    f.send(p, json!({"op":"test_directory_key","key":"delete"}));
    inspect(&f, p);
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "clear_activity_history")
            .count(),
        deletions,
        "Delete must keep normal text editing semantics"
    );
    control(&f, p, "act_filter:all");
    f.send(p, json!({"op":"test_directory_key","key":"down"}));
    let s = inspect(&f, p);
    assert_eq!(s["directory_selected_id"], "personal:sig_read");
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
    f.send(p, json!({"op":"test_directory_key","key":"delete"}));
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "clear_activity_history"
                && v["hub"] == "personal"
                && v["signal_id"] == "personal:sig_read"
        })
    });
    assert!(node(&s, "directory_items", "reply_activity:personal:sig_read")["hidden"] == true);
    control(&f, p, "reply_activity:personal:sig_read");
    f.send(p, json!({"op":"test_directory_key","key":"return"}));
    assert_eq!(inspect(&f, p)["composer_open"], false);
    f.send(p, json!({"op":"test_directory_key","key":"up"}));
    assert_eq!(
        inspect(&f, p)["directory_selected_id"],
        "personal:sig_unread"
    );
    control(&f, p, "act_filter:unread");
    assert!(node(
        &inspect(&f, p),
        "directory_items",
        "activity_row:personal:sig_read"
    )
    .is_null());
    control(&f, p, "reply_activity:personal:sig_unread");
    let s = inspect(&f, p);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:sig_unread");
    assert_eq!(
        node(&s, "directory_items", "directory_shortcuts")["text"],
        "↵ Send    ⎋ Cancel Reply"
    );
    f.send(p, json!({"op":"test_composer_input","value":"Approved"}));
    control(&f, p, "composer_send");
    wait(|| {
        f.frames(p).iter().any(|v| {
            v["op"] == "send_signal"
                && v["in_reply_to"] == "personal:sig_unread"
                && v["message"] == "Approved"
        })
    });
    control(&f, p, "composer_cancel");
    let queries = f
        .frames(p)
        .iter()
        .filter(|v| v["op"] == "query_directory")
        .count();
    control(&f, p, "activity_mark_all_read");
    let read = inspect(&f, p);
    assert_eq!(read["unread_count"], 0);
    assert!(nodes(&read["directory_items"]).iter().any(|n| n["text"]
        .as_str()
        .is_some_and(|s| s.ends_with(" · 0 unread"))));
    assert_eq!(
        f.frames(p)
            .iter()
            .filter(|v| v["op"] == "query_directory")
            .count(),
        queries + 1
    );
    wait(|| {
        f.frames(p)
            .iter()
            .any(|v| v["op"] == "ack_signals" && v["ack_all"] == true)
    });
    control(&f, p, "act_filter:all");
    for (reason, label) in [
        ("stale_generation", "Session reloaded"),
        ("driver_unavailable", "App unavailable"),
        ("session_switched", "Session changed"),
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
            node(&snapshot, "directory_items", "act_held:personal:sig_unread")["text"],
            format!("Held: {label}")
        );
        assert_eq!(
            node(&snapshot, "directory_items", "act_held:personal:sig_unread")["hidden"],
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
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
    let scrolls = ["active_scroll", "my_remote_scroll", "available_scroll"];
    for id in scrolls {
        f.send(p, json!({"op":"test_scroll","id":id,"offset":20}));
    }
    let scrolled = inspect(&f, p);
    for id in scrolls {
        assert_eq!(node(&scrolled, "items", id)["content_y"], 20, "{id}");
    }
    frame["unread_count"] = json!(1);
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    for id in scrolls {
        assert_eq!(
            node(&refreshed, "items", id)["content_y"],
            20,
            "{id} must survive a rebuild"
        );
    }
    for (field, scroll_id, delta) in [
        (
            "listeners",
            "active_scroll",
            node(&refreshed, "items", "session:personal:row-0")["height"]
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
            node(&inspect(&f, p), "items", scroll_id)["content_y"]
                .as_f64()
                .unwrap(),
            20.0 + delta,
            "{scroll_id} preserves its viewed row after insertion"
        );
        frame[field].as_array_mut().unwrap().remove(0);
        f.send(p, frame.clone());
        assert_eq!(node(&inspect(&f, p), "items", scroll_id)["content_y"], 20);
    }
    // A new expanded group changes identifiers, never the existing group's key.
    let mut expanded = frame.clone();
    for i in 0..6 {
        expanded["available"].as_array_mut().unwrap().push(json!({"harness":"second","session_id":format!("other-{i}"),"title":"Review","cwd":"/tmp","endpoint_pid":4242}));
    }
    f.send(p, expanded);
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll:fixture")["content_y"],
        20
    );
    control(&f, p, "toggle_group:second");
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        20
    );
    control(&f, p, "toggle_group:second");
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll:fixture")["content_y"],
        20
    );
    let mut inserted = frame["available"][0].clone();
    inserted["session_id"] = json!("inserted");
    frame["available"]
        .as_array_mut()
        .unwrap()
        .insert(0, inserted);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        64,
        "Keep candidate-0 at the same screen position after an insertion"
    );
    frame["available"][0]["needs_reload"] = json!(true);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        78,
        "Keep the anchor after a row above it grows"
    );
    frame["available"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "items", "available_scroll")["content_y"],
        20
    );
    control(&f, p, "open_directory");
    f.send(p,json!({"op":"error","source_op":"listen_session","error":"BRIDGE_UNAVAILABLE: Retry listening"}));
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!node(&inspect(&f, p), "items", "menu_error_row").is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":["work: Hub temporarily unavailable"]}),
    );
    let warned = inspect(&f, p);
    assert_eq!(
        node(&warned, "directory_items", "directory_warning")["hidden"],
        false
    );
    assert!(
        node(&warned, "directory_items", "directory_warning")["tooltip"]
            .as_str()
            .unwrap()
            .contains("work")
    );
    assert_eq!(
        node(&warned, "directory_items", "directory_scroll")["height"],
        306
    );
    f.send(p,json!({"op":"error","source_op":"query_directory","error":"CONNECTION_FAILED: Remote Hub unavailable (os error 61)"}));
    let failed = inspect(&f, p);
    assert!(
        node(&failed, "directory_items", "directory_warning")["text"]
            .as_str()
            .unwrap()
            .starts_with("Cannot refresh")
    );
    assert!(
        !node(&failed, "directory_items", "dir_peer:personal:one").is_null(),
        "A failed query must preserve usable rows"
    );
    assert!(!node(&failed, "items", "menu_error_row").is_null());
    send_fixture(
        &f,
        p,
        json!({"op":"directory","peers":[peer("one")],"warnings":[]}),
    );
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true
    );
    control(&f, p, "open_settings");
    f.send(
        p,
        json!({"op":"error","source_op":"hub_action","error":"Hub temporarily unavailable"}),
    );
    f.send(p, json!({"op":"test_directory_query","query":""}));
    f.send(p, json!({"op":"test_menu_open"}));
    assert!(!node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
    control(&f, p, "dismiss_settings_error");
    assert!(node(&inspect(&f, p), "settings_items", "settings_error_banner").is_null());
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
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        172,
        "Directory insertion preserves the viewed peer"
    );
    send_fixture(&f, p, json!({"op":"directory","peers":peers}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
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
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        120
    );
    frame["activity"][11]["message"] = json!("New details");
    f.send(p, frame.clone());
    let refreshed = inspect(&f, p);
    assert_eq!(refreshed["directory_selected_id"], "sig-0");
    assert_eq!(
        node(&refreshed, "directory_items", "directory_scroll")["content_y"],
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
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        180,
        "Insertion above the viewport preserves the viewed Activity row"
    );
    frame["activity"].as_array_mut().unwrap().remove(0);
    f.send(p, frame.clone());
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
        120,
        "Removing the inserted row restores the same anchor"
    );
    control(&f, p, "reply_activity:sig-0");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_scroll")["content_y"],
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
    for op in [
        "clear_activity_history",
        "ack_signals",
        "mark_all_activity_read",
        "copy_activity_message",
    ] {
        // Legacy untagged failures retain their routing; tagged overlap is exercised below.
        f.send(
            p,
            json!({"op":"error","source_op":op,"error":format!("Could not complete {op}")}),
        );
        let s = inspect(&f, p);
        assert_eq!(
            node(&s, "directory_items", "directory_warning")["hidden"],
            false
        );
        assert_eq!(
            node(&s, "directory_items", "directory_warning")["text"],
            "Cannot update Activity. Please try again."
        );
        assert!(node(&s, "directory_items", "directory_warning")["tooltip"]
            .as_str()
            .unwrap()
            .contains(op));
        assert!(node(&s, "settings_items", "settings_error_banner").is_null());
        let request = f
            .frames(p)
            .into_iter()
            .rev()
            .find(|r| r["op"] == "query_directory" || r["op"] == "open_directory")
            .unwrap();
        f.send(p,json!({"op":"error","source_op":"query_directory","request_id":request["request_id"],"error":"Directory lookup failed"}));
        assert!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["tooltip"]
                .as_str()
                .unwrap()
                .contains(op),
            "Action error takes precedence over query error"
        );
        f.send(p,json!({"op":"directory","request_id":request["request_id"],"query":request["query"],"scope":request["scope"],"harness":request["harness"],"peers":[],"warnings":[]}));
        assert!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["tooltip"]
                .as_str()
                .unwrap()
                .contains(op),
            "Passive Directory success must retain the action error"
        );
        control(&f, p, "dir_mode:directory");
        assert_eq!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
            true
        );
        control(&f, p, "dir_mode:activity");
    }
    // Retry B owns feedback before either response arrives.
    let latest_delete = || {
        inspect(&f, p);
        f.frames(p)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "clear_activity_history")
            .unwrap()
    };
    control(&f, p, "delete_activity:sig-0");
    let a = latest_delete();
    control(&f, p, "delete_activity:sig-0");
    let b = latest_delete();
    assert!(!a["request_id"].as_str().unwrap().is_empty());
    assert_ne!(a["request_id"], b["request_id"]);
    for op in ["error", "state"] {
        f.send(p,json!({"op":op,"ok":false,"source_op":"clear_activity_history","request_id":a["request_id"],"error":"Late failure from A"}));
        assert_eq!(
            node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
            true,
            "Superseded {op} failure must not own B's feedback"
        );
    }
    f.send(p,json!({"op":"state","ok":false,"source_op":"clear_activity_history","request_id":b["request_id"],"error":"Current failure from B"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false,
        "Matching ok:false mutation results must be visible"
    );
    let mut success = frame.clone();
    success["ok"] = json!(true);
    success["source_op"] = json!("clear_activity_history");
    success["request_id"] = a["request_id"].clone();
    f.send(p, success.clone());
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false,
        "A's success cannot settle B's error"
    );
    success["request_id"] = b["request_id"].clone();
    f.send(p, success);
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true,
        "B's matching state success settles its feedback"
    );
    f.send(p,json!({"op":"error","source_op":"clear_activity_history","request_id":a["request_id"],"error":"A after B completed"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true
    );
    control(&f, p, "delete_activity:sig-0");
    let c = latest_delete();
    f.send(p,json!({"op":"error","source_op":"clear_activity_history","request_id":c["request_id"],"error":"Current failure from C"}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        false
    );
    f.send(p,json!({"op":"directory","ok":true,"source_op":"clear_activity_history","request_id":c["request_id"],"peers":[]}));
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_warning")["hidden"],
        true,
        "A matching Directory receipt also settles the action independently of query correlation"
    );
}

#[test]
fn appkit_directory_keyboard_navigation_shortcuts_and_preserved_peers() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let p = start(&f);
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
    assert_eq!(
        node(&s, "directory_items", "dir_scope:mine")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "dir_harness:fixture")["border_width"],
        0
    );
    assert_eq!(
        node(&s, "directory_items", "dir_scope:all")["border_width"],
        1
    );
    assert_eq!(node(&s, "directory_items", "directory_panel")["width"], 560);
    for id in [
        "directory_scope_track",
        "directory_harness_scroll",
        "directory_mode_track",
    ] {
        assert!(
            (node(&s, "directory_items", id)["background"][3]
                .as_f64()
                .unwrap()
                - 0.06)
                .abs()
                < 0.001
        );
    }
    for membership in ["listeners", "hubs"] {
        let queries = f
            .frames(p)
            .iter()
            .filter(|v| v["op"] == "query_directory")
            .count();
        let mut changed = state();
        if membership == "listeners" {
            changed["listeners"] = json!([]);
        } else {
            changed["hubs"][0]["enabled"] = json!(false);
        }
        f.send(p, changed);
        inspect(&f, p);
        assert_eq!(
            f.frames(p)
                .iter()
                .filter(|v| v["op"] == "query_directory")
                .count(),
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
        assert_eq!(
            node(&s, "directory_items", "composer_input")["text"],
            oversized
        );
        assert_eq!(
            node(&s, "directory_items", "composer_error")["text"],
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
    assert_eq!(
        node(&s, "directory_items", "composer_send")["text"],
        "Sending…"
    );
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
    let submitted = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":submitted["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], false);
    assert_eq!(node(&s, "directory_items", "composer_input")["text"], "");
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
    let failed = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    assert!(!failed["request_id"].as_str().unwrap().is_empty());
    f.send(
        p,
        json!({"op":"composer_result","ok":false,"error":"Hub unavailable. Retry.","request_id":failed["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Draft preserved"
    );
    assert_eq!(
        node(&s, "directory_items", "composer_error")["text"],
        "Hub unavailable. Retry."
    );
    // A's trailing generic error can arrive after B has already been submitted.
    control(&f, p, "composer_send");
    inspect(&f, p);
    let retry = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|v| v["op"] == "send_signal")
        .unwrap();
    assert_ne!(retry["request_id"], failed["request_id"]);
    for source_op in ["send_signal", "send_reply_from_human"] {
        f.send(p, json!({"op":"error","source_op":source_op,"request_id":failed["request_id"],"error":"Late send error"}));
    }
    f.send(p, json!({"op":"composer_result","ok":false,"request_id":failed["request_id"],"error":"Late result"}));
    for frame in [
        json!({"op":"composer_result","ok":true}),
        json!({"op":"composer_result","ok":false,"error":"Untagged result"}),
        json!({"op":"error","error":"INVALID_INPUT: Message is too large to send"}),
        json!({"op":"error","source_op":"send_signal","error":"Untagged send error"}),
        json!({"op":"error","source_op":"send_reply_from_human","error":"Untagged reply error"}),
    ] {
        f.send(p, frame);
    }
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(
        node(&s, "directory_items", "composer_send")["text"],
        "Sending…"
    );
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
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Next draft"
    );
    assert_eq!(node(&s, "directory_items", "composer_error")["text"], "");
    control(&f, p, "composer_cancel");
    assert_eq!(
        node(&inspect(&f, p), "directory_items", "directory_shortcuts")["text"],
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
    assert_eq!(replies[0]["in_reply_to"], "personal:reply_a");
    control(&f, p, "reply_activity:personal:reply_b");
    let s = inspect(&f, p);
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
    f.send(
        p,
        json!({"op":"composer_result","ok":true,"request_id":replies[0]["request_id"]}),
    );
    let s = inspect(&f, p);
    assert_eq!(s["composer_open"], true);
    assert_eq!(s["composer_target"], "personal:one");
    assert_eq!(s["composer_reply_signal_id"], "personal:reply_b");
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
    assert_eq!(
        node(&s, "directory_items", "composer_send")["text"],
        "✓ Sent"
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
    assert_eq!(replies[1]["in_reply_to"], "personal:reply_b");
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
    assert_eq!(
        node(&s, "directory_items", "composer_input")["text"],
        "Same reply text"
    );
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
    f.send(p, state());
    assert!(!node(&inspect(&f, p), "directory_items", "msg_dir:personal:one").is_null());
    f.send(p, json!({"op":"test_directory_key","key":"escape"}));
    assert_eq!(inspect(&f, p)["directory_open"], false);
}
#[test]
fn v29_full_message_copy_is_correlated_and_never_copies_a_preview() {
    let _lock = serial();
    let f = Fixture::new(json!({}));
    let meta = f.provider("delivered_unconfirmed");
    let mut l = ClosedLoop::new(&f);
    l.until(|s| node(s, "items", "header").is_object());
    // Cache a complete listener collection before long activity previews mark
    // subsequent passive snapshots truncated.
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
    assert_eq!(
        node(&copied, "directory_items", &control_id)["text"],
        "✓ Copied"
    );
    l.control(&control_id);
    l.pump();
    f.send(
        l.ui,
        json!({"op":"test_expire_copy_feedback","id":control_id}),
    );
    let restored = inspect(&f, l.ui);
    let button = node(&restored, "directory_items", &control_id);
    assert_eq!(button["text"], "Copy");
    assert_eq!(button["accessibility_label"], "Copy message");
    let frames: Vec<_> = l
        .frames
        .iter()
        .filter(|f| f["op"] == "activity_message")
        .collect();
    assert!(frames.len() > 1);
    assert!(frames
        .iter()
        .all(|f| serde_json::to_vec(f).unwrap().len() < 262_144));
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
        "A newer clipboard action supersedes in-flight message chunks"
    );
    assert_eq!(inspect(&f, l.ui)["composer_open"], false);
    l.control(&control_id);
    let request = f
        .frames(l.ui)
        .into_iter()
        .rev()
        .find(|a| a["op"] == "copy_activity_message")
        .unwrap();
    l.control("dir_mode:directory");
    f.send(l.ui,json!({"op":"activity_message","request_id":request["request_id"],"part":0,"message":message.trim(),"complete":true}));
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
    l.until(|s| !node(s, "directory_items", &control_id).is_null());
    for invalid_chunk in [false, true] {
        l.control(&control_id);
        let request = f
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "copy_activity_message")
            .unwrap();
        f.send(l.ui, if invalid_chunk {
            json!({"op":"activity_message","request_id":request["request_id"],"part":1,"message":"out of order","complete":true,"error":"COPY_FAILED: Message unavailable"})
        } else {
            json!({"op":"error","source_op":"copy_activity_message","request_id":request["request_id"],"error":"COPY_FAILED: Message unavailable"})
        });
        let failed = inspect(&f, l.ui);
        assert_eq!(failed["clipboard"], expected);
        let status = node(&failed, "directory_items", "directory_summary");
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
        let warning = node(&inspect(&f, l.ui), "directory_items", "directory_warning");
        assert_eq!(warning["hidden"], false);
        assert_eq!(warning["tooltip"], "Message unavailable");
        l.control(&control_id);
        let success = l.until(|s| s["clipboard"] == message.trim());
        assert_eq!(
            node(&success, "directory_items", "directory_warning")["hidden"],
            true,
            "Only the successful correlated retry settles the copy error"
        );
        assert_eq!(
            node(&success, "directory_items", "directory_summary")["tooltip"],
            ""
        );
        l.control(address["id"].as_str().unwrap());
        assert_eq!(inspect(&f, l.ui)["clipboard"], expected);
    }
    // This final clipboard arbitration section uses a complete synthetic menu
    // snapshot; the daemon snapshot includes intentionally truncated activity.
    let mut state = super::state();
    state["listeners"] = json!([]);
    state["settings"] = f.action(json!({"op":"get_snapshot"})).1["settings"].clone();
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
            .frames(l.ui)
            .into_iter()
            .rev()
            .find(|a| a["op"] == "copy_activity_message")
            .unwrap();
        l.control(id);
        f.send(l.ui, json!({"op":"activity_message","request_id":request["request_id"],"part":0,"message":"late full message","complete":true}));
        let copied = inspect(&f, l.ui);
        assert_eq!(copied["clipboard"], expected, "{id}");
        if id == "copy_hub_invite" {
            assert_eq!(
                node(&copied, "settings_items", id)["text"],
                "✓ Invite Copied"
            );
        }
    }
    // Real production actions must emit non-coalesced success receipts, not just passive snapshots.
    l.pump();
    for op in [
        "ack_signals",
        "mark_activity_read",
        "mark_all_activity_read",
        "clear_activity_history",
    ] {
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
            node(&inspect(&f, l.ui), "directory_items", "directory_warning")["hidden"],
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
