#!/usr/bin/env python3
"""A local, durable protocol example. A real transport would send the same RPC to its host.

wtm runs this once per request: one JSON-RPC 2.0 object on stdin, one response on stdout, exit 0.
Every failure is answered as a JSON-RPC error rather than a crash or a non-zero exit, because wtm
discards the output of a helper that exits non-zero and reports only its exit code.
"""
import fcntl
import json
import os
from pathlib import Path
import sys
import time
import uuid

LEASE_SECONDS = 10
HISTORY = 200


class DemoError(Exception):
    """A request this backend refuses, reported to the caller as a JSON-RPC error."""


def snapshot(session, client, after):
    reset = after < max(0, session["cursor"] - HISTORY)
    events = session["events"][-HISTORY:]
    return {"cursor": session["cursor"], "reset": reset,
            "events": [e for e in events if reset or e["seq"] > after],
            "lease": session["lease"] if session["owner"] == client else None}


def cursor_of(params):
    after = params.get("after", 0)
    if not isinstance(after, int) or isinstance(after, bool) or after < 0:
        raise DemoError("after must be a non-negative integer")
    return after


def renew(session, now):
    # Renewed only when half the lease is gone, so a controller's poll every second does not
    # rewrite the state file every second.
    if session["expires"] - now < LEASE_SECONDS / 2:
        session["expires"] = now + LEASE_SECONDS


def dispatch(state, method, params):
    now = time.time()
    client = params.get("client")
    if not isinstance(client, str) or not client:
        raise DemoError("A client identity is required")
    if method == "list":
        return {"sessions": [{"id": s["id"], "title": s["title"], "host": "This computer"}
                             for s in state["sessions"].values() if not s["stopped"]]}
    if method == "create":
        key = str(uuid.uuid4())
        state["sessions"][key] = new_session(key)
        return {"id": key}
    session = state["sessions"].get(params.get("session"))
    if not session or session["stopped"]:
        raise DemoError("Session no longer exists; reconnect does not create one")
    if session["owner"] is not None and session["expires"] < now:
        session["owner"] = None
        session["lease"] = None
    if method in ("attach", "poll"):
        after = cursor_of(params)
        if session["owner"] == client:
            renew(session, now)
        return snapshot(session, client, after)
    if method == "control":
        if params.get("release"):
            if session["owner"] != client or session["lease"] != params.get("lease"):
                raise DemoError("The input lease is no longer yours")
            session["owner"] = None
            session["lease"] = None
        elif session["owner"] not in (None, client):
            raise DemoError("Another client controls input; ask it to release control")
        else:
            session["owner"] = client
            session["lease"] = str(uuid.uuid4())
            session["expires"] = now + LEASE_SECONDS
        return {"lease": session["lease"]}
    if method == "detach":
        if session["owner"] == client:
            session["owner"] = None
            session["lease"] = None
        return {"detached": True}
    if session["owner"] != client or session["lease"] != params.get("lease"):
        raise DemoError("A current input lease is required")
    if method == "stop":
        session["stopped"] = True
        return {"stopped": True}
    if method == "input":
        key = params.get("input_id")
        if not isinstance(key, str) or not key:
            raise DemoError("input_id is required for deduplication")
        if key in session["inputs"]:
            return {"accepted": key}
        if len(session["inputs"]) >= 1000:
            raise DemoError("Demo input limit reached; create another demo session")
        text = params.get("text")
        if not isinstance(text, str) or len(text) > 8192:
            raise DemoError("Demo input must be at most 8192 characters")
        session["inputs"].append(key)
        session["cursor"] += 1
        session["events"].append({"seq": session["cursor"], "text": "Echo: " + text})
        session["events"] = session["events"][-HISTORY:]
        return {"accepted": key}
    raise DemoError("This backend does not support " + str(method))


def new_session(key):
    return {"id": key, "title": "Durable echo · protocol demonstration", "cursor": 1,
            "events": [{"seq": 1, "text": "This is a durable echo session, not an AI agent. "
                                          "Detach and reattach to the same id."}],
            "owner": None, "lease": None, "expires": 0, "inputs": [], "stopped": False}


def load(file):
    """The saved state, or a fresh one. A damaged file is set aside rather than overwritten."""
    if not file.exists():
        return {"sessions": {"demo-1": new_session("demo-1")}}
    try:
        state = json.loads(file.read_text())
        if not isinstance(state, dict) or not isinstance(state.get("sessions"), dict):
            raise ValueError("unexpected shape")
        return state
    except (OSError, ValueError) as error:
        file.replace(file.with_name("sessions.corrupt.json"))
        raise DemoError("sessions.json was unreadable (" + str(error) + ") and was moved to "
                        "sessions.corrupt.json; the next request starts a fresh demo") from error


def handle(request):
    if not isinstance(request, dict):
        raise DemoError("A request must be a JSON object")
    if request.get("jsonrpc") != "2.0" or request.get("api") != 1:
        raise DemoError("Unsupported session API")
    method = request.get("method")
    params = request.get("params", {})
    if not isinstance(params, dict):
        raise DemoError("params must be an object")
    root = Path(os.environ["WTM_PLUGIN_DATA"])
    root.mkdir(parents=True, exist_ok=True)
    with (root / "session.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        file = root / "sessions.json"
        state = load(file)
        before = json.dumps(state, sort_keys=True)
        result = dispatch(state, method, params)
        # Read-only requests, and polls that renew nothing, leave the file alone.
        if json.dumps(state, sort_keys=True) != before:
            temp = root / "sessions.tmp"
            temp.write_text(json.dumps(state))
            temp.replace(file)
        return result


def main():
    response = {"jsonrpc": "2.0", "id": None}
    try:
        request = json.load(sys.stdin)
    except ValueError as error:
        response["error"] = {"code": -32700, "message": "Request is not JSON: " + str(error)}
        print(json.dumps(response))
        return
    if isinstance(request, dict):
        response["id"] = request.get("id")
    try:
        response["result"] = handle(request)
    except DemoError as error:
        response["error"] = {"code": -32000, "message": str(error)}
    except Exception as error:  # Still an answer: a crash would reach the user as an exit code.
        response["error"] = {"code": -32603, "message": "Demo backend failed: " + repr(error)}
    print(json.dumps(response))


if __name__ == "__main__":
    main()
