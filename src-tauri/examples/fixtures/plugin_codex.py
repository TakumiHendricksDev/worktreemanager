#!/usr/bin/env python3
"""Isolated protocol fixture: no model, credentials, or user conversation access."""
import copy
import json
import pathlib
import sys
import uuid

if 'generate-json-schema' in sys.argv:
    output = pathlib.Path(sys.argv[sys.argv.index('--out') + 1]) / 'v2'
    output.mkdir(parents=True)
    (output / 'ThreadForkParams.json').write_text(json.dumps({'properties': {'lastTurnId': {'type': 'string'}}}))
    sys.exit(0)

authoring = pathlib.Path.cwd().name == 'generated'
storage = pathlib.Path('.git/provider.json')
thread = None

def load():
    return json.loads(storage.read_text()) if storage.exists() else {}

def send(value):
    print(json.dumps(value), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method', '')
    params = request.get('params', {})
    if 'id' not in request:
        continue
    result = {}
    data = load()
    if method == 'thread/start':
        thread = 'native-author-thread' if authoring else str(uuid.uuid4())
        data[thread] = {'id': thread, 'turns': []}
        result = {'thread': data[thread]}
    if method == 'thread/resume':
        thread = params['threadId']
        result = {'thread': data[thread]}
    if method == 'thread/fork':
        source = data[params['threadId']]
        boundary = next(i for i, turn in enumerate(source['turns']) if turn['id'] == params['lastTurnId'])
        thread = str(uuid.uuid4())
        data[thread] = {'id': thread, 'turns': copy.deepcopy(source['turns'][:boundary + 1])}
        result = {'thread': data[thread]}
        pathlib.Path('.git/fork.json').write_text(json.dumps(params))
    if method == 'skills/list':
        result = {'data': []}
    if method == 'turn/start':
        with open('.git/author-turns.jsonl', 'a') as output:
            output.write(json.dumps(request) + '\n')
        if authoring:
            pathlib.Path('wtm-plugin.toml').write_text("schema=1\napi=1\nid='example.generated-check'\nname='Generated check'\nversion='0.1.0'\n[[panels]]\nid='panel'\ntitle='Panel'\nfile='panel.html'\n")
            pathlib.Path('panel.html').write_text('<!doctype html><p>Native authoring fixture</p>')
        turn_id = str(uuid.uuid4())
        item = {'id': str(uuid.uuid4()), 'type': 'userMessage', 'content': params['input']}
        turn = {'id': turn_id, 'status': 'completed', 'items': [item]}
        data[thread]['turns'].append(turn)
        result = {'turn': turn}
    if method in ('thread/start', 'thread/fork', 'turn/start'):
        storage.write_text(json.dumps(data))
    send({'id': request['id'], 'result': result})
    if method == 'turn/start':
        send({'method': 'turn/started', 'params': {'threadId': thread, 'turn': {'id': turn_id, 'status': 'inProgress'}}})
        send({'method': 'item/completed', 'params': {'threadId': thread, 'turnId': turn_id, 'item': item}})
        send({'method': 'turn/completed', 'params': {'threadId': thread, 'turn': turn}})
