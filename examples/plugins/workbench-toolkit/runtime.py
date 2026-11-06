"""A native activation example. Stdout contains only the versioned JSON-RPC protocol."""
import json
import sys

state = {}
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "activate":
        state["activation"] = request.get("params", {})
    elif method == "event":
        event = request.get("params", {})
        state[event.get("name", "unknown")] = event.get("payload")
    elif "id" in request:
        if method == "plugin.status":
            result = {"active": True, "observed": list(state)}
        elif method == "hook":
            result = {}
        else:
            print(json.dumps({"id": request["id"], "error": {"message": "Unknown method"}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
