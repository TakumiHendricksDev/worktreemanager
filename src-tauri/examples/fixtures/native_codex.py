#!/usr/bin/env python3
"""Deterministic public-protocol fixture. No model, command execution, or permission approval."""
import base64
import json
import os
import pathlib
import socket
import struct
import sys
import uuid

args = sys.argv[1:]
if '--version' in args:
    print('codex-cli 0.165.0 (isolated protocol fixture)')
    sys.exit()
if '--help' in args:
    print('--remote --stdio')
    sys.exit()

def record(value):
    with pathlib.Path('.git/native-wire.jsonl').open('a') as log:
        log.write(json.dumps(value) + '\n')

def output(value):
    print(json.dumps(value), flush=True)

if 'app-server' in args:
    for line in sys.stdin:
        msg = json.loads(line)
        record(msg)
        method = msg.get('method')
        if method == 'initialize':
            output({'id':msg['id'], 'result':{'userAgent':'native-fixture'}})
        elif method in ['thread/start', 'thread/resume']:
            output({'id':msg['id'], 'result':{'thread':{'id':'fixture-conversation', 'status':{'type':'idle'}}, 'model':'fixture', 'sandbox':{'type':'readOnly'}, 'approvalPolicy':'on-request'}})
        elif method == 'turn/start':
            pathlib.Path('.git/native-items.json').write_text(json.dumps([{'type':'userMessage','id':'fixture-message','clientId':msg['params']['clientUserMessageId'],'content':msg['params']['input']}]))
            if not pathlib.Path('.git/drop-native-reply').exists():
                output({'id':msg['id'], 'result':{'turn':{'id':'fixture-turn'}}})
            output({'method':'turn/started','params':{'threadId':'fixture-conversation','turn':{'id':'fixture-turn'}}})
            output({'id':99,'method':'item/commandExecution/requestApproval','params':{'threadId':'fixture-conversation','turnId':'fixture-turn','itemId':'fixture-item','command':'example command'}})
        elif msg.get('id') == 99:
            output({'method':'turn/completed','params':{'threadId':'fixture-conversation','turn':{'id':'fixture-turn','status':'completed','error':None}}})
        elif method == 'thread/read':
            items = json.loads(pathlib.Path('.git/native-items.json').read_text())
            output({'id':msg['id'], 'result':{'thread':{'id':'fixture-conversation','turns':[{'items':items}]}}})
    sys.exit()

address = args[args.index('--remote') + 1].removeprefix('unix://')
connection = socket.socket(socket.AF_UNIX)
connection.connect(address)
key = base64.b64encode(uuid.uuid4().bytes).decode()
connection.sendall(('GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+key+'\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
header = b''
while not header.endswith(b'\r\n\r\n'):
    header += connection.recv(1)
assert b'101' in header

def send(value):
    data = json.dumps(value).encode()
    mask = os.urandom(4)
    length = len(data)
    prefix = bytes([0x81, 0x80 | length]) if length < 126 else bytes([0x81, 0x80 | 126]) + struct.pack('!H', length)
    connection.sendall(prefix + mask + bytes(b ^ mask[n % 4] for n, b in enumerate(data)))

def read(n):
    data = b''
    while len(data) < n:
        chunk = connection.recv(n-len(data))
        if not chunk:
            raise EOFError()
        data += chunk
    return data

send({'id':1, 'method':'initialize', 'params':{'clientInfo':{'name':'native-fixture','version':'1'}, 'capabilities':{'experimentalApi':True}}})
while True:
    first, second = read(2)
    size = second & 127
    if size == 126: size = struct.unpack('!H', read(2))[0]
    elif size == 127: size = struct.unpack('!Q', read(8))[0]
    data = read(size)
    if first & 15 == 8: break
    if first & 15 != 1: continue
    message = json.loads(data)
    if message.get('id') == 1:
        send({'method':'initialized'})
        send({'id':2, 'method':'thread/start', 'params':{'developerInstructions':'User context', 'cwd':os.getcwd()}})
    elif message.get('method') == 'item/commandExecution/requestApproval':
        record({'nativePermissionPrompt':message['id']})
        # The fixture TUI, never the host, decides; denial must be forwarded unchanged.
        send({'id':message['id'], 'result':{'decision':'decline'}})
