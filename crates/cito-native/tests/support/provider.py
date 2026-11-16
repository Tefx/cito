# purpose: exercise native plugin requests through a real isolated NDJSON subprocess.
# usage: python3 driver.py (installed by the Rust integration fixture).
# effects: reads options.json and appends calls.jsonl only inside the test sandbox.
# requires: Python 3 and cleanroom HOME/CITO_STATE_DIR/CITO_CONFIG_PATH.
import json
import os
import pathlib
import sys
import time

root = pathlib.Path(__file__).parent
request = json.loads(sys.stdin.readline())
options = json.loads((root / "options.json").read_text())
for key in ["HOME", "CITO_STATE_DIR", "CITO_CONFIG_PATH"]:
    assert pathlib.Path(os.environ[key]).resolve().is_relative_to(root.parents[2].resolve())
    if key in request.get("env", {}):
        assert request["env"][key] == os.environ[key]
with (root / "calls.jsonl").open("a") as output:
    output.write(json.dumps(dict(request, process_env={key: os.environ[key] for key in ["CODEX_THREAD_ID", "CLAUDE_SESSION_ID", "PI_CODING_AGENT_SESSION_FILE", "CITO_ENDPOINT_PID"] if key in os.environ})) + "\n")
caller = dict(harness="fixture", session_id=options["session_id"], cwd=options.get("cwd", str(root)), title=options.get("title", "Native fixture"), endpoint_pid=os.getppid())
op = request["op"]
result = dict(ok=True)
if op == "resolve_caller":
    result = dict(ok=True, caller=caller) if request.get("meta", {}).get("expected_harness") == "fixture" else dict(ok=False, error_code="BINDING_REQUIRED", message="Explicit fixture identity required")
elif op == "discover_sessions":
    if options.get("discover_delay_ms"):
        time.sleep(options["discover_delay_ms"] / 1000)
    if options.get("discover_error"):
        result = dict(ok=False, error_code="DISCOVERY_UNAVAILABLE", message="Discovery is temporarily unavailable")
    else:
        result["sessions"] = options.get("sessions", [dict(caller, updated_at_ms=int(time.time()*1000))])
elif op == "probe":
    result.update(status=options["status"], current_session_id=options["session_id"])
elif op == "wake":
    result["outcome"] = options["outcome"]
elif op == "setup_inspect":
    if options.get("setup_gate"):
        deadline = time.monotonic() + options.get("setup_gate_timeout_ms", 2000) / 1000
        while not pathlib.Path(options["setup_gate"]).exists() and time.monotonic() < deadline:
            time.sleep(0.005)
    delay_ms = options.get("setup_delay_ms", 0)
    if delay_ms:
        time.sleep(delay_ms / 1000)
    result.update(installed=True, configured=options.get('configured', True), paths=[])
elif op in ["setup_apply", "setup_remove"]:
    result["actions"] = []
print(json.dumps(result), flush=True)
