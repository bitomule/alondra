#!/usr/bin/python3
import json
import sys
import urllib.request

endpoint = sys.argv[1]
for line in sys.stdin:
    message = json.loads(line)
    identity, method = message.get('id'), message.get('method')
    if identity is None:
        continue
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'serverInfo': {'name': 'alondra-cell-proof', 'version': '1'}, 'capabilities': {'tools': {}}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'barrier', 'description': 'Wait for the native-cell fixture barrier.', 'inputSchema': {'type': 'object', 'properties': {'phase': {'type': 'string', 'enum': ['first', 'second']}}, 'required': ['phase'], 'additionalProperties': False}}]}
    elif method == 'tools/call' and message.get('params', {}).get('name') == 'barrier':
        phase = message['params']['arguments']['phase']
        request = urllib.request.Request(endpoint + '/mcp/' + phase, data=b'{}', headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(request) as response:
            value = json.load(response)
        result = {'content': [{'type': 'text', 'text': json.dumps(value)}]}
    else:
        print(json.dumps({'jsonrpc': '2.0', 'id': identity, 'error': {'code': -32601, 'message': 'Unsupported fixture method'}}), flush=True)
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': identity, 'result': result}), flush=True)
