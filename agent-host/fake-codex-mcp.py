#!/usr/bin/python3
import json, os, sys
trace_path = sys.argv[1] if len(sys.argv) > 1 and os.path.isabs(sys.argv[1]) else os.environ.get('ALONDRA_FAKE_MCP_TRACE')
for line in sys.stdin:
    message = json.loads(line)
    identity, method = message.get('id'), message.get('method')
    if identity is None: continue
    if trace_path:
        with open(trace_path, 'a') as trace: trace.write(json.dumps({'method':method,'params':message.get('params',{}), **({'environment': {key:os.environ.get(key) for key in ['ALONDRA_HOME','ALONDRA_DOMAIN_CAPABILITY','ALONDRA_PORT','ALONDRA_PROVIDER']}} if method == 'initialize' else {})}) + '\n')
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'serverInfo': {'name':'alondra-proof','version':'1'}, 'capabilities': {'tools': {}}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'description': 'Bounded fixture read', 'inputSchema': {'type':'object','properties':{'path':{'type':'string'}},'additionalProperties':False}} for name in ['read_file','glob_files','search_files','read_skill']]}
    elif method == 'tools/call':
        result = {'content':[{'type':'text','text':'Read-only Alondra fixture result.'}]}
    else:
        print(json.dumps({'jsonrpc':'2.0','id':identity,'error':{'code':-32601,'message':'unsupported method: '+str(method)}}), flush=True)
        continue
    print(json.dumps({'jsonrpc':'2.0','id':identity,'result':result}), flush=True)
