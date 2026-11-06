#!/usr/bin/env python3
"""Lifecycle/MCP fixture. The continuation file stands for an explicit user submission."""
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import time

args = sys.argv[1:]
if '--version' in args:
    print('2.1.318 (isolated Claude hook fixture)')
    sys.exit()
if '--help' in args:
    print('--settings --mcp-config --append-system-prompt')
    sys.exit()
settings = json.loads(args[args.index('--settings') + 1])
server = json.loads(args[args.index('--mcp-config') + 1])['mcpServers']['wtm']
conversation = args[args.index('--session-id') + 1] if '--session-id' in args else args[args.index('--resume') + 1]

def hook(name, **data):
    command = settings['hooks'][name][0]['hooks'][0]['command']
    value = {'hook_event_name':name, 'cwd':os.getcwd(), 'session_id':conversation, 'permission_mode':'default', **data}
    result = subprocess.run(shlex.split(command), input=json.dumps(value), capture_output=True, text=True, timeout=15, check=True)
    return json.loads(result.stdout)

hook('SessionStart', model='fixture')
while not pathlib.Path('.git/continue-native').exists():
    time.sleep(0.05)
reply = hook('UserPromptSubmit', prompt='Explicit fixture continuation')
context = reply['hookSpecificOutput']['additionalContext']
pathlib.Path('.git/collected-native.txt').write_text(context)
receipt = re.search(r'with receipt ([a-f0-9-]{36})', context).group(1)
bridge = subprocess.Popen([server['command'], *server['args']], env={**os.environ, **server['env']}, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
try:
    def call(identifier, method, params):
        bridge.stdin.write(json.dumps({'jsonrpc':'2.0','id':identifier,'method':method,'params':params})+'\n')
        bridge.stdin.flush()
        answer = json.loads(bridge.stdout.readline())
        assert answer.get('id') == identifier and 'error' not in answer
        return answer['result']
    call(1, 'initialize', {'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'native-fixture','version':'1'}})
    listed = call(2, 'tools/list', {})
    assert any(t['name'] == 'native_acknowledge' for t in listed['tools'])
    acknowledged = call(3, 'tools/call', {'name':'native_acknowledge','arguments':{'receipt':receipt}})
    assert not acknowledged.get('isError', False)
finally:
    bridge.terminate()
    bridge.wait(timeout=3)
hook('Stop')
hook('SessionEnd', reason='clear')
conversation = 'fixture-replacement-conversation'
hook('SessionStart', source='clear', model='fixture')
while True:
    time.sleep(1)
