#[path = "support/daemon/mod.rs"]
mod daemon;
#[path = "support/distribution.rs"]
mod distribution;
#[path = "support/plugins.rs"]
mod plugins;
mod support;
#[path = "support/ui/mod.rs"]
mod ui;

#[test]
fn hub_http_errors_keep_retryability_through_tools_call() {
    use serde_json::{json, Value};
    use support::{serial, wait, Fixture};

    let _lock = serial();
    let f = Fixture::new(json!({}));
    let remote = f.spawn(
        "python3",
        &[
            "-u",
            "-c",
            r#"import http.server, json
class Hub(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path == '/v1/rpc' and request['op'] == 'kv_snapshot':
            with open('snapshot-error.json') as f:
                status, code = json.load(f)
            body = {'ok': False, 'error': {'code': code, 'message': 'Snapshot fixture'}}
        else:
            status, body = 200, {'ok': True, 'result': {}}
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)
server = http.server.HTTPServer(('127.0.0.1', 0), Hub)
print(json.dumps({'port': server.server_port}), flush=True)
server.serve_forever()
"#,
        ],
        &[],
    );
    wait(|| !f.frames(remote).is_empty());
    let url = format!("http://127.0.0.1:{}", f.frames(remote)[0]["port"]);
    std::fs::write(&f.config, json!({"default_hub":"local","hubs":{"local":{"url":"local://"},"work":{"url":url,"authMode":"none"}}}).to_string()).unwrap();
    let error_file = f.dir.join("snapshot-error.json");
    std::fs::write(&error_file, "[503,\"UNAVAILABLE\"]").unwrap();
    let meta = f.provider("completed");
    let daemon = f.daemon(&[]);
    for (status, code, retryable) in [
        (500, "INTERNAL_ERROR", true),
        (503, "UNAVAILABLE", true),
        (403, "FORBIDDEN", false),
    ] {
        let pending = error_file.with_extension("tmp");
        std::fs::write(&pending, json!([status, code]).to_string()).unwrap();
        std::fs::rename(pending, &error_file).unwrap();
        let result = f.tool_result(
            "cito_send",
            json!({"hub":"work","to":"@bob","message":"hello"}),
            meta.clone(),
        );
        assert_eq!(result["isError"], true, "{result}");
        let content: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            content,
            json!({"ok":false,"error":{"code":code,"message":"Snapshot fixture","retryable":retryable}})
        );
    }
    f.stop(daemon);
    f.stop(remote);
}
