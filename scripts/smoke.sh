#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p tagos-mail-assistant-demo --offline
BIN="./target/debug/tagos-mail-assistant-demo"
export BIN
python3 - <<'PY'
import json, os, socket, subprocess, time, urllib.request

bin_path = os.environ["BIN"]

def assert_ok(label, payload):
    if "error" in payload:
        raise SystemExit(f"{label} error: {payload['error']}")
    return payload.get("result", payload)

def stdio_session():
    proc = subprocess.Popen(
        [bin_path, "stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    next_id = 1

    def call(method, params, note=False):
        nonlocal next_id
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if not note:
            message["id"] = next_id
            next_id += 1
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        if note:
            return None
        while True:
            line = proc.stdout.readline()
            if not line:
                err = proc.stderr.read()
                raise SystemExit(f"stdio closed during {method}: {err}")
            if not line.strip():
                continue
            payload = json.loads(line)
            if "id" in payload:
                return assert_ok(method, payload)

    init = call("initialize", {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "smoke", "version": "0"},
    })
    version = init["protocolVersion"]
    if version != "2025-11-25":
        raise SystemExit(f"stdio protocol {version}")
    call("notifications/initialized", {}, note=True)
    listed = call("tools/list", {})
    names = [tool["name"] for tool in listed["tools"]]
    if "inbox_briefing" not in names or any(name.startswith("send") for name in names):
        raise SystemExit(f"stdio tools: {names}")
    briefing = call("tools/call", {"name": "inbox_briefing", "arguments": {}})
    text = json.dumps(briefing)
    if "Priya Shah" not in text:
        raise SystemExit(f"stdio briefing missing Priya: {text[:400]}")
    tracked = call("tools/call", {"name": "await_reply", "arguments": {
        "person": "Priya",
        "due": "2026-10-23",
        "about": "the contract",
        "thread_id": "11111111-1111-4111-8111-111111111111",
        "tz": "America/New_York",
    }})
    followup_id = json.loads(tracked["content"][0]["text"])["followup"]["id"]
    call("tools/call", {"name": "demo_advance_clock", "arguments": {"hours": 107}})
    draft = call("tools/call", {"name": "prepare_reply_draft", "arguments": {
        "followup_id": followup_id,
        "kind": "nudge",
    }})
    body = json.loads(draft["content"][0]["text"])
    if body.get("sent") is not False or "Nothing was sent" not in body.get("note", ""):
        raise SystemExit(f"stdio draft sent or missing note: {body}")
    proc.stdin.close()
    proc.terminate()
    proc.wait(timeout=5)
    listed_cli = subprocess.check_output(
        [
            "npx", "--yes", "@modelcontextprotocol/inspector@2.9.0", "--cli",
            bin_path, "stdio",
            "--method", "tools/list",
            "--format", "json",
        ],
        text=True,
    )
    if "inbox_briefing" not in listed_cli or "send_message" in listed_cli:
        raise SystemExit(f"inspector stdio tools: {listed_cli[:500]}")
    print("inspector stdio ok")
    print("stdio ok")

def free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port

def http_session():
    port = free_port()
    proc = subprocess.Popen(
        [bin_path, "serve", "--port", str(port)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    url = f"http://127.0.0.1:{port}/mcp"
    deadline = time.time() + 5
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(proc.stderr.read())
        try:
            urllib.request.urlopen(url, timeout=0.2)
        except urllib.error.HTTPError:
            break
        except Exception:
            time.sleep(0.05)
    def post(payload):
        data = json.dumps(payload).encode()
        req = urllib.request.Request(url, data=data, method="POST", headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        })
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.loads(resp.read().decode())
    init = assert_ok("initialize", post({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"},
        },
    }))
    if init["protocolVersion"] != "2025-11-25":
        raise SystemExit(init)
    listed = assert_ok("tools/list", post({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {},
    }))
    names = [tool["name"] for tool in listed["tools"]]
    if "demo_reset" not in names or any("send" in name for name in names):
        raise SystemExit(f"http tools: {names}")
    get = urllib.request.Request(url, method="GET")
    try:
        urllib.request.urlopen(get, timeout=5)
        raise SystemExit("GET /mcp was allowed")
    except urllib.error.HTTPError as err:
        if err.code != 405:
            raise SystemExit(f"GET /mcp status {err.code}")
    briefing = assert_ok("briefing", post({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "inbox_briefing", "arguments": {}},
    }))
    if "Priya Shah" not in json.dumps(briefing):
        raise SystemExit("http briefing missing Priya")
    inspector = ["npx", "--yes", "@modelcontextprotocol/inspector@2.9.0", "--cli", f"http://127.0.0.1:{port}/mcp"]
    listed_cli = subprocess.check_output(
        inspector + ["--method", "tools/list", "--format", "json"],
        text=True,
    )
    if "inbox_briefing" not in listed_cli or "send_message" in listed_cli:
        raise SystemExit(f"inspector tools/list: {listed_cli[:500]}")
    brief_cli = subprocess.check_output(
        inspector + [
            "--method", "tools/call",
            "--tool-name", "inbox_briefing",
            "--tool-args-json", "{}",
            "--format", "json",
        ],
        text=True,
    )
    if "Priya Shah" not in brief_cli:
        raise SystemExit(f"inspector briefing: {brief_cli[:500]}")
    print("inspector http ok")
    proc.terminate()
    proc.wait(timeout=5)
    print("http ok")

stdio_session()
http_session()
print("smoke ok")
PY
