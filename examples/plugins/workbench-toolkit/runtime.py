"""A native activation example. Stdout carries only newline-delimited JSON-RPC 2.0 frames.

wtm starts this once when the package is enabled and keeps it running. Anything written to stderr
is shown on the package's page in Settings → Plugins, so diagnostics go there, never to stdout.
"""
import json
import sys

state = {}


def send(frame):
    print(json.dumps({"jsonrpc": "2.0", **frame}), flush=True)


def reply_error(request_id, code, message):
    send({"id": request_id, "error": {"code": code, "message": message}})


for line in sys.stdin:
    if not line.strip():
        continue
    try:
        request = json.loads(line)
    except ValueError:
        # No id can be read from it, so there is nobody to answer; say so where people look.
        print("Ignored a frame that is not JSON", file=sys.stderr, flush=True)
        continue
    if not isinstance(request, dict):
        print("Ignored a frame that is not a JSON object", file=sys.stderr, flush=True)
        continue
    method = request.get("method")
    params = request.get("params")
    if not isinstance(params, dict):
        params = {}
    if method is None:
        # A reply to a `host` request. This example never makes one, so there is nothing to match.
        continue
    if "id" not in request:
        # Notifications: no reply is expected.
        if method == "activate":
            state["activation"] = params
        elif method == "event":
            state[str(params.get("name", "unknown"))] = params.get("payload")
        continue
    if method == "plugin.status":
        send({"id": request["id"], "result": {"active": True, "observed": sorted(state)}})
    elif method == "hook":
        # An empty result lets the operation go ahead with its arguments unchanged.
        send({"id": request["id"], "result": {}})
    else:
        reply_error(request["id"], -32601, "Unknown method " + str(method))
