use super::support::*;
use serde_json::{json, Value};
use std::fs;

/// Real daemon subprocess with its production dispatch_action path and UI mailbox.
/// The recording endpoint preserves daemon frame order; no state/settings/result
/// frames are synthesized by this relay. All children use Fixture's cleanroom API.
struct ClosedLoop<'a> {
    f: &'a Fixture,
    ui: usize,
    actions: usize,
    frames: Vec<Value>,
    results: Vec<(Value, u16, Value)>,
}
impl<'a> ClosedLoop<'a> {
    fn new(f: &'a Fixture) -> Self {
        let recorder = f.recording_ui();
        f.daemon(&[
            ("CITO_DISABLE_UI", "0"),
            ("CITO_UI_BINARY", recorder.to_str().unwrap()),
        ]);
        Self {
            f,
            ui: start(f),
            actions: 0,
            frames: Vec::new(),
            results: Vec::new(),
        }
    }
    fn pump(&mut self) {
        let actions = self.f.frames(self.ui);
        for action in &actions[self.actions..] {
            let (status, result) = self.f.action(action.clone());
            self.results.push((action.clone(), status, result));
        }
        self.actions = actions.len();
        let frames: Vec<Value> = fs::read_to_string(self.f.home.join("ui-frames"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        for frame in &frames[self.frames.len()..] {
            self.f.send(self.ui, frame.clone());
            inspect(self.f, self.ui);
        }
        self.frames = frames;
    }
    fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        loop {
            self.pump();
            let s = inspect(self.f, self.ui);
            if predicate(&s) {
                return s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Closed-loop timeout: {s}; results={:?}",
                self.results
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn control(&mut self, id: &str) {
        control(self.f, self.ui, id);
        inspect(self.f, self.ui);
    }
    fn input(&mut self, id: &str, value: &str) {
        self.f.send(
            self.ui,
            json!({"op":"test_settings_input","id":id,"value":value}),
        );
        inspect(self.f, self.ui);
    }
    fn snapshot(&mut self, surface: &str, name: &str) {
        self.pump();
        inspect(self.f, self.ui);
        render_png(self.f, self.ui, surface, name);
        inspect(self.f, self.ui);
    }
}

fn render_png(f: &Fixture, p: usize, surface: &str, name: &str) {
    if std::env::var_os("CITO_TEST_SNAPSHOT_DIR").is_some() {
        let stem = name.strip_suffix(".png").unwrap_or(name);
        for appearance in ["light", "dark"] {
            render_png_as(
                f,
                p,
                surface,
                &format!("{stem}_{appearance}.png"),
                Some(appearance),
            );
        }
    } else {
        render_png_as(f, p, surface, name, None);
    }
}
fn render_png_as(f: &Fixture, p: usize, surface: &str, name: &str, appearance: Option<&str>) {
    inspect_as(f, p, appearance);
    let Some(dir) = std::env::var_os("CITO_TEST_SNAPSHOT_DIR") else {
        return;
    };
    fs::create_dir_all(&dir).unwrap();
    let path = std::path::Path::new(&dir).join(name);
    f.send(
        p,
        json!({"op":"test_render_png","surface":surface,"path":path,"appearance":appearance}),
    );
    wait(|| {
        f.output(p, true)
            .contains(&format!("rendered {}", path.display()))
    });
    let data = fs::read(&path).unwrap();
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    assert!(data.len() > 1000);
    let width = u32::from_be_bytes(data[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(data[20..24].try_into().unwrap());
    assert_eq!(
        width,
        if surface == "settings" {
            1040
        } else if surface == "directory" {
            1120
        } else {
            760
        }
    );
    if surface == "settings" {
        assert_eq!(height, 920);
    }
    assert!(height > 100);
    inspect_as(f, p, appearance);
}

fn hubs() -> Value {
    json!([{"name":"personal","status":"connected","user":"alice","auth_mode":"oidc","error":null,"is_builtin":false,"supports_workspace_scope":true,"human_can_set_scope":true,"can_login":false},{"name":"work","status":"connected","user":"alice","auth_mode":"none","error":null,"is_builtin":false,"supports_workspace_scope":true,"human_can_set_scope":true,"can_login":false}])
}
fn listener() -> Value {
    json!({"hub":"personal","listener_id":"c_abcdefghijkl","public_id":"repo-abcdefghijkl","scoped_id":"personal:repo-abcdefghijkl","display_address":"alice/fixture#abcd","harness":"fixture","session_id":"s1","title":"café conversation","cwd":"/tmp","repo":null,"scope":"self","is_self_principal":true,"can_set_scope":true,"can_pin":true,"can_rename":true,"can_stop":true})
}
fn state() -> Value {
    json!({"op":"state","hubs":hubs(),"listeners":[listener()],"harnesses":[{"id":"fixture","display_name":"Fixture","sf_symbol":"terminal","tint":"purple","tier":"stable","order":0}],"unread_count":0,"directory_online_count":2})
}
fn start(f: &Fixture) -> usize {
    let ui = root().join("bin/cito-ui-darwin-arm64");
    f.spawn(ui.to_str().unwrap(), &[], &[])
}
fn inspect(f: &Fixture, p: usize) -> Value {
    inspect_as(f, p, None)
}
fn inspect_unchecked(f: &Fixture, p: usize, appearance: Option<&str>) -> Value {
    let count = f
        .output(p, true)
        .lines()
        .filter(|l| l.starts_with("cito-ui: menu "))
        .count();
    f.send(p, json!({"op":"test_inspect_menu","appearance":appearance}));
    wait(|| {
        f.output(p, true)
            .lines()
            .filter(|l| l.starts_with("cito-ui: menu "))
            .count()
            > count
    });
    let output = f.output(p, true);
    let snapshot: Value = serde_json::from_str(
        output
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("cito-ui: menu "))
            .unwrap(),
    )
    .unwrap();
    snapshot
}
fn inspect_as(f: &Fixture, p: usize, appearance: Option<&str>) -> Value {
    let snapshot = inspect_unchecked(f, p, appearance);
    assert_eq!(
        snapshot["truncated_labels"],
        json!([]),
        "{:?}",
        ["items", "directory_items", "settings_items"]
            .into_iter()
            .flat_map(|key| nodes(&snapshot[key]))
            .filter(|n| snapshot["truncated_labels"]
                .as_array()
                .is_some_and(|ids| ids.contains(&n["id"])))
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot["overlapping_siblings"], json!([]));
    assert_eq!(snapshot["clipped_views"], json!([]));
    for key in [
        "tight_padding",
        "non_anchored_action_bars",
        "low_contrast_labels",
        "jargon_leaks",
        "address_mismatches",
    ] {
        assert_eq!(snapshot[key], json!([]), "{key}");
    }
    snapshot
}
fn nodes(tree: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    if let Some(rows) = tree.as_array() {
        for node in rows {
            result.push(node.clone());
            result.extend(nodes(&node["children"]));
        }
    }
    result
}
fn node(snapshot: &Value, surface: &str, id: &str) -> Value {
    nodes(&snapshot[surface])
        .into_iter()
        .find(|v| v["id"] == id)
        .unwrap_or(Value::Null)
}
fn control(f: &Fixture, p: usize, id: &str) {
    f.send(p, json!({"op":"test_control","id":id}));
}
fn confirm_channel_action(f: &Fixture, p: usize, op: &str) {
    inspect(f, p);
    let action = f
        .frames(p)
        .into_iter()
        .rev()
        .find(|a| a["op"] == op)
        .unwrap();
    f.send(p, json!({"op":"settings","ok":true,"committed":true,"hub":action["hub"],"channel_id":action["channel_id"],"source_op":op,"request_id":action["request_id"]}));
}
/// Synthetic directory frames echo the real UI request, just as the daemon does.
fn send_fixture(f: &Fixture, p: usize, mut frame: Value) {
    if frame["op"] == "directory" && frame["request_id"].is_null() {
        inspect(f, p);
        if let Some(request) = f
            .frames(p)
            .iter()
            .rev()
            .find(|a| matches!(a["op"].as_str(), Some("query_directory" | "open_directory")))
        {
            for key in ["request_id", "query", "scope", "harness"] {
                frame[key] = request[key].clone();
            }
        }
    }
    f.send(p, frame);
}
fn peer(id: &str) -> Value {
    json!({"hub":"personal","listener_id":format!("c_{id}"),"display_address":format!("bob/fixture#{}", &cito_native::core::identity::hash(id)[..4]),"public_id":id,"scoped_id":format!("personal:{id}"),"user":"bob","host_id":"remote","host_name":"laptop","harness":"fixture","title":id,"about":"Schema work","repo":"acme/service","branch":"main","scope":"workspace","last_seen_at":"2030-01-01T00:00:00Z","is_self_principal":false,"channels":["laptop"]})
}
fn settings() -> Value {
    json!({"apps":[{"id":"pi","display_name":"Pi","tier":"stable","installed":true,"configured":true,"status":"configured","detail":"Connected","paths":["/tmp/settings.json"]},{"id":"codex","display_name":"Codex","tier":"stable","installed":true,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]},{"id":"cursor","display_name":"Cursor","tier":"experimental","installed":false,"configured":false,"status":"not_configured","detail":"Click Set Up to connect","paths":[]}],"channels":[{"id":"channel","display_name":"Human Channel","sf_symbol":"paperplane.fill","tint":"blue","enabled":true,"configured":false,"status":"hub_ready","detail":"Ready","hub_provided":false,"pairing_supported":true,"has_hub_service":true,"hub_ready":true,"can_configure_hub":true,"can_clear_hub":true,"can_edit_personal":true,"can_pair":true,"can_test":true,"can_unpair":true,"error_origin":null,"fields":[{"key":"secret","scope":"hub","label":"Secret","type":"secret","required":true,"placeholder":"Token","value":"secret","configured":true},{"key":"account","scope":"user","label":"Account","type":"string","required":true,"placeholder":"Account","value":"","configured":false}]}],"hubs":[{"name":"personal","url":"local://","auth_mode":"none","is_builtin":true,"supports_workspace_scope":false,"can_login":false,"enabled":true,"is_default":true,"status":"connected","user":"alice","error":null},{"name":"work","url":"http://127.0.0.1:4291","auth_mode":"none","is_builtin":false,"supports_workspace_scope":true,"can_login":false,"enabled":true,"is_default":false,"status":"connected","user":"alice","error":null}],"host_hub":{"enabled":false,"running":false,"port":4291,"endpoint_url":"http://127.0.0.1:4291","token":"","invite_command":"","error":null},"launch_at_login":false})
}

mod capabilities;
mod directory;
mod lazy_rendering;
mod menu;
mod oidc_settlement;
mod oracle_e2e;
mod render_stability;
mod settings_channels;
mod settings_hubs;
mod settlement;
