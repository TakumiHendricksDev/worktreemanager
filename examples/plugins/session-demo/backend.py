#!/usr/bin/env python3
"""A local, durable protocol example. A real transport would send the same RPC to its host."""
import fcntl
import json
import os
from pathlib import Path
import sys
import time
import uuid


def snapshot(session, client, after):
    reset = after < max(0, session["cursor"] - 200)
    events = session["events"][-200:]
    return {"cursor": session["cursor"], "reset": reset,
            "events": [e for e in events if reset or e["seq"] > after],
            "lease": session["lease"] if session["owner"] == client else None}


def dispatch(state, method, params):
    now = time.time()
    client = params.get("client")
    if not isinstance(client, str) or not client:
        raise ValueError("A client identity is required")
    if method == "list":
        return {"sessions": [{"id": s["id"], "title": s["title"], "host": "This computer"}
                             for s in state["sessions"].values() if not s["stopped"]]}
    if method == "create":
        key = str(uuid.uuid4())
        state["sessions"][key] = new_session(key)
        return {"id": key}
    session = state["sessions"].get(params.get("session"))
    if not session or session["stopped"]:
        raise ValueError("Session no longer exists; reconnect does not create one")
    if session["expires"] < now:
        session["owner"] = None
        session["lease"] = None
    if method in ("attach", "poll"):
        if session["owner"] == client:
            session["expires"] = now + 10
        return snapshot(session, client, params.get("after", 0))
    if method == "control":
        if params.get("release"):
            if session["owner"] != client or session["lease"] != params.get("lease"):
                raise ValueError("The input lease is no longer yours")
            session["owner"] = None
            session["lease"] = None
        elif session["owner"] not in (None, client):
            raise ValueError("Another client controls input; ask it to release control")
        else:
            session["owner"] = client
            session["lease"] = str(uuid.uuid4())
            session["expires"] = now + 10
        return {"lease": session["lease"]}
    if method == "detach":
        if session["owner"] == client:
            session["owner"] = None
            session["lease"] = None
        return {"detached": True}
    if session["owner"] != client or session["lease"] != params.get("lease"):
        raise ValueError("A current input lease is required")
    if method == "stop":
        session["stopped"] = True
        return {"stopped": True}
    if method == "input":
        key = params.get("input_id")
        if not isinstance(key, str) or not key:
            raise ValueError("input_id is required for deduplication")
        if key in session["inputs"]:
            return {"accepted": key}
        if len(session["inputs"]) >= 1000:
            raise ValueError("Demo input limit reached; create another demo session")
        text = params.get("text")
        if not isinstance(text, str) or len(text) > 8192:
            raise ValueError("Demo input must be at most 8192 characters")
        session["inputs"].append(key)
        session["cursor"] += 1
        session["events"].append({"seq": session["cursor"], "text": "Echo: " + text})
        session["events"] = session["events"][-200:]
        return {"accepted": key}
    raise ValueError("This backend does not support " + method)


def new_session(key):
    return {"id": key, "title": "Durable echo · protocol demonstration", "cursor": 1,
            "events": [{"seq": 1, "text": "This is a durable echo session, not an AI agent. Detach and reattach to the same id."}],
            "owner": None, "lease": None, "expires": 0, "inputs": [], "stopped": False}


def main():
    request = json.load(sys.stdin)
    root = Path(os.environ["WTM_PLUGIN_DATA"])
    root.mkdir(parents=True, exist_ok=True)
    with (root / "session.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        file = root / "sessions.json"
        state = json.loads(file.read_text()) if file.exists() else {"sessions": {"demo-1": new_session("demo-1")}}
        response = {"jsonrpc": "2.0", "id": request["id"]}
        try:
            if request.get("api") != 1:
                raise ValueError("Unsupported session API")
            response["result"] = dispatch(state, request["method"], request.get("params", {}))
        except (ValueError, TypeError) as error:
            response["error"] = {"code": -32000, "message": str(error)}
        temp = root / "sessions.tmp"
        temp.write_text(json.dumps(state))
        temp.replace(file)
        print(json.dumps(response))


if __name__ == "__main__":
    main()
