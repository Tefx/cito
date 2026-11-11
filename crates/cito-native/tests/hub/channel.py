# purpose: Exercise the persistent HumanChannel wire protocol without external services.
# usage: python3 channel.py (spawned by native HubEngine regression tests)
# effects: Writes requests to frames.jsonl in the disposable plugin working directory.
# requires: Python 3 standard library.
import json
import sys

failed = set()
for line in sys.stdin:
    request = json.loads(line)
    with open("frames.jsonl", "a", encoding="utf-8") as stream:
        stream.write(json.dumps(request) + "\n")
    op = request.get("op")
    if op == "shutdown":
        break
    if op == "configure":
        continue
    if op == "deliver":
        sid = request["signal"]["signal_id"]
        ok = sid != "fail-once" or sid in failed
        failed.add(sid)
        reply = {"op": "delivered", "delivery_id": request["delivery_id"], "ok": ok,
                 "external_msg_id": sid + "-2", "external_msg_ids": [sid + "-1", sid + "-2"]}
    elif op == "format_pair":
        reply = {"op": "rpc_result", "req_id": request["req_id"], "ok": True,
                 "instruction": "Send " + request["code"] + " to fixture-account"}
    elif op == "test":
        address = request.get("user_config", {}).get("address", "")
        if address.startswith("redeem:"):
            print(json.dumps({"op": "pair_redeem", "code": address.removeprefix("redeem:"),
                              "is_private_chat": True, "external_chat_id": "room",
                              "external_user_id": "alice", "display_label": "Alice",
                              "user_config_patch": {"address": "paired-room"}}), flush=True)
        if request.get("hub_config", {}).get("credential") == "invalid":
            reply = {"op": "rpc_result", "req_id": request["req_id"], "ok": False, "error": "Invalid token"}
        else:
            reply = {"op": "rpc_result", "req_id": request["req_id"], "ok": True, "detail": "Fixture test succeeded"}
    else:
        continue
    print(json.dumps(reply), flush=True)
