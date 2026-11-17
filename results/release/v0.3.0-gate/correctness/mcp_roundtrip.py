#!/usr/bin/env python3
"""Minimal MCP stdio round trip: initialize -> notifications/initialized -> tools/list."""
import json, os, subprocess, sys

bin_path = sys.argv[1]
apk = sys.argv[2]
env = {k: v for k, v in os.environ.items() if not k.startswith('ACOS_')}
env.update({
    'ACOS_RUNTIME_DIR': os.path.abspath('.cache/rg1/rt-smoke'),
    'ACOS_SESSION_SOCKET': os.path.abspath('.cache/rg1/rt-smoke/sock'),
})
p = subprocess.Popen([bin_path], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.PIPE, text=True, cwd='/tmp', env=env)

def send(obj):
    p.stdin.write(json.dumps(obj) + '\n'); p.stdin.flush()

def recv():
    while True:
        line = p.stdout.readline()
        if not line:
            raise SystemExit('server closed stdout; stderr=' + p.stderr.read())
        line = line.strip()
        if not line:
            continue
        print('RESP ' + line, flush=True)
        return json.loads(line)

send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                 "clientInfo": {"name": "relgate1-smoke", "version": "0.0.1"}}})
init = recv()
print('CHECK initialize serverInfo=%r protocolVersion=%r' % (
    (init.get('result') or {}).get('serverInfo'), (init.get('result') or {}).get('protocolVersion')), flush=True)
send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tl = recv()
tools = (tl.get('result') or {}).get('tools') or []
names = sorted(t['name'] for t in tools)
print('CHECK tools_list count=%d names=%s' % (len(tools), ','.join(names)), flush=True)
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
      "params": {"name": "acos_help", "arguments": {"topic": "workflow"}}})
help_reply = recv()
txt = ((help_reply.get('result') or {}).get('content') or [{}])[0].get('text', '')
print('CHECK acos_help bytes=%d' % len(txt), flush=True)
ok = bool(init.get('result')) and len(tools) >= 8 and 'acos_verify' in names and len(txt) > 100
print('SMOKE ' + ('ok' if ok else 'fail'), flush=True)
p.stdin.close()
try:
    p.wait(timeout=10)
except subprocess.TimeoutExpired:
    p.kill()
sys.exit(0 if ok else 1)
