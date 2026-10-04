#!/usr/bin/env bash
# Drive MCP Inspector 2.9.0 through the demo consent page for both auth profiles.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p tagos-mail-assistant-demo --offline
BIN="./target/debug/tagos-mail-assistant-demo"
export MCP_AUTO_OPEN_ENABLED=true
export BROWSER=true

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  if [[ -n "${INSPECTOR_PID:-}" ]]; then kill "$INSPECTOR_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

for profile in mcp alexa; do
  PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
  "$BIN" serve --port "$PORT" --auth-profile "$profile" >"/tmp/tagos-demo-$profile.log" 2>&1 &
  SERVER_PID=$!
  sleep 0.3
  python3 - "$PORT" "$profile" <<'PY'
import sys, urllib.request
port, profile = sys.argv[1], sys.argv[2]
url = f"http://127.0.0.1:{port}/mcp"
data = b'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
req = urllib.request.Request(url, data=data, method="POST", headers={
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
})
try:
    urllib.request.urlopen(req, timeout=3)
    raise SystemExit("expected 401")
except urllib.error.HTTPError as err:
    header = err.headers.get("WWW-Authenticate")
    if profile == "mcp" and (err.code != 401 or not header or "resource_metadata" not in header):
        raise SystemExit(f"mcp 401 header missing: {err.code} {header}")
    if profile == "alexa" and (err.code != 401 or header):
        raise SystemExit(f"alexa 401 should have no header: {err.code} {header}")
print(f"{profile} 401 ok")
PY
  : > /tmp/tagos-insp.out
  : > /tmp/tagos-insp.err
  npx --yes @modelcontextprotocol/inspector@2.9.0 --cli \
    "http://127.0.0.1:$PORT/mcp" \
    --client-id demo-inspector \
    --callback-url http://127.0.0.1:6276/oauth/callback \
    --relogin \
    --method tools/list \
    --format json > /tmp/tagos-insp.out 2> /tmp/tagos-insp.err &
  INSPECTOR_PID=$!
  python3 - "$PORT" <<'PY'
import pathlib, re, sys, time, urllib.parse, urllib.request
port = sys.argv[1]
errp = pathlib.Path("/tmp/tagos-insp.err")
url = None
for _ in range(80):
    text = re.sub(r"\x1b\[[0-9;]*m", "", errp.read_text(errors="replace"))
    if "Please navigate to:" in text:
        url = text.split("Please navigate to:", 1)[1].strip().split()[0]
        break
    time.sleep(0.2)
if not url or "/demo-oauth/authorize" not in url:
    raise SystemExit(errp.read_text(errors="replace")[:800] or "no authorize URL")
html = urllib.request.urlopen(url, timeout=5).read().decode()
if "Link Tagos Mail (demo)" not in html:
    raise SystemExit("consent page missing the button")
fields = dict(re.findall(r'name="([^"]+)" value="([^"]*)"', html))
req = urllib.request.Request(
    f"http://127.0.0.1:{port}/demo-oauth/authorize",
    data=urllib.parse.urlencode(fields).encode(),
    method="POST",
)
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None
try:
    urllib.request.build_opener(NoRedirect).open(req, timeout=5)
except urllib.error.HTTPError as err:
    location = err.headers.get("Location")
    if err.code != 303 or not location:
        raise SystemExit(f"consent redirect {err.code}")
    urllib.request.urlopen(location, timeout=5).read()
for _ in range(50):
    out = pathlib.Path("/tmp/tagos-insp.out").read_text(errors="replace")
    if "inbox_briefing" in out and "send_message" not in out:
        print("inspector tools/list ok")
        break
    time.sleep(0.2)
else:
    raise SystemExit(pathlib.Path("/tmp/tagos-insp.err").read_text(errors="replace")[-800:])
PY
  kill "$INSPECTOR_PID" "$SERVER_PID" 2>/dev/null || true
  wait "$INSPECTOR_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  INSPECTOR_PID=""
  SERVER_PID=""
  echo "$profile inspector oauth ok"
done
echo "oauth inspector ok"
