#!/usr/bin/env python3
"""Opt-in mixed Mac/client connection capacity profiles, not production defaults."""
import argparse
import json
import math
import signal
from pathlib import Path
import sys

COUNTS = (1000, 5000, 10000)

def profile(connections):
    if type(connections) is not int or connections not in COUNTS:
        raise ValueError('capacity preset must be 1000, 5000 or 10000')
    macs = connections // 3
    clients = connections - macs
    resources = {
        1000: {'relay_cpus': 1, 'relay_memory_mib': 512, 'client_cpus': 2, 'client_memory_mib': 512},
        5000: {'relay_cpus': 2, 'relay_memory_mib': 1024, 'client_cpus': 2, 'client_memory_mib': 1024},
        10000: {'relay_cpus': 2, 'relay_memory_mib': 1536, 'client_cpus': 2, 'client_memory_mib': 2048},
    }[connections]
    return {
        'kind': 'capacity', 'limits': f'capacity-{connections}', 'gate': 'sustained',
        'connections': connections, 'accounts': 16, 'macs': macs, 'clients': clients,
        'matrix': {'clients_per_mac': 2, 'extra_client_macs': connections % 3,
                   'assignment': 'first remainder Macs receive one additional client; owner=Mac index modulo 16'},
        'duration_seconds': 600, 'wait_seconds': 20, 'burst_requests': 10000,
        'burst_concurrency': 64, 'ramp_requests_per_second': 100,
        'short_latency_budget_ms': {'p95': 500, 'p99': 2000},
        'compiled_limits': {'changes': clients+32, 'work': macs+16, 'requests': connections+256},
        'server_waiter_gate': {
            'thresholds': {name: (count*80+99)//100 for name,count in
                           [('changes',clients),('work',macs),('total',connections)]},
            'steady_fraction_percent': 80, 'continuous_ms': 5000, 'fresh_timeout_ms': 3000},
        'harness_budgets': {'preparation_ms': 1200000, 'ramp_ms': 180000,
                            'client_overhead_ms': 1650000, 'pressure_grace_ms': 35000,
                            'recovery_ms': 120000, 'restart_ms': 30000, 'startup_ms': 30000},
        'resource_limits': {**resources, 'postgres_memory_mib': 512, 'keycloak_memory_mib': 1200,
                            'container_nofile': 65536, 'min_docker_cpus': 4,
                            'min_docker_memory_mib': sum([resources['relay_memory_mib'],resources['client_memory_mib'],512,1200,1024])},
        'freshness': 'private_challenge_before_capture_v1',
        'auth': {'initial_grants': 16, 'refresh_seconds': 30, 'sharing': 'one grant per account, no login per GET or per device'},
        'scope': 'real local PG/Keycloak/relay; unique synthetic Mac/client signing identities; derived lab limits; no native/Claude/APNs or Pi capacity claim',
    }

def compare(before, after):
    """Compare only complete passed runs with the same preset and environment."""
    def case(result):
        if result.get('status')!='passed' or result.get('source',{}).get('dirty') is not False or not result.get('source',{}).get('commit'):
            raise ValueError('capacity comparison requires passed clean committed results')
        cases=[x for x in result.get('cases',[]) if x.get('phase')=='capacity']
        if len(cases)!=1 or cases[0].get('status')!='passed' or not cases[0].get('steady_passed') or not cases[0].get('recovery_passed') or cases[0].get('short_latency',{}).get('passed') is not True:
            raise ValueError('capacity steady and recovery evidence must both pass')
        if result.get('load_configuration')!=profile(result['load_configuration']['connections']):
            raise ValueError('capacity comparison requires an unchanged versioned preset')
        resources=result.get('resources',{})
        terminal=resources.get('client_terminal_state',{})
        if (resources.get('sample_errors') != 0 or resources.get('sampler_joined') is not True
                or terminal.get('running') is not False
                or terminal.get('oom_killed') is not False or terminal.get('exit_code') != 0
                or resources.get('limits') != result['load_configuration']['resource_limits']):
            raise ValueError('failed or incomplete capacity resource evidence')
        return cases[0]
    old,new=case(before),case(after)
    for name in ('hardware','versions','load_configuration','instrumentation'):
        if not before.get(name) or before[name]!=after.get(name):
            raise ValueError('incomparable capacity '+name)
    if before['resources']['limits']!=after['resources']['limits']:
        raise ValueError('incomparable resource budgets')
    def metrics(result,item):
        relay=result['resources']['observations']
        client=result['resources']['client_observations']
        idle=[row for row in relay if row['phase']=='idle_before_restart']
        steady=[row for row in relay if row['phase']=='steady']
        generated=[row for row in client if row['phase']=='steady']
        if not idle or not steady or not generated:
            raise ValueError('generator/steady/pre-restart evidence missing')
        return {'short_p95_ms':item['bursts']['p95_ms'],'short_p99_ms':item['bursts']['p99_ms'],
                'idle_relay_rss_kib':int(idle[-1]['process']['VmRSS'].split()[0]),'idle_relay_fds':idle[-1]['open_fds'],
                'steady_relay_cpu_percent':sum(float(row['cpu_percent'].rstrip('%')) for row in steady)/len(steady),
                'steady_generator_cpu_percent':sum(float(row['cpu_percent'].rstrip('%')) for row in generated)/len(generated),
                'steady_generator_peak_rss_kib':max(int(row['process']['VmRSS'].split()[0]) for row in generated),
                'recovery_ms':item['recovery']['elapsed_ms']}
    a,b=metrics(before,old),metrics(after,new)
    if any(type(value) not in (int,float) or not math.isfinite(value) or value < 0
           for value in (*a.values(),*b.values())):
        raise ValueError('invalid capacity metric')
    floors={'short_p95_ms':5,'short_p99_ms':10,'idle_relay_rss_kib':8192,'idle_relay_fds':10,
            'steady_relay_cpu_percent':10,'steady_generator_cpu_percent':10,'steady_generator_peak_rss_kib':8192,'recovery_ms':2000}
    changes=[{'metric':name,'before':value,'after':b[name],'regression':b[name]-value>max(floors[name],abs(value)*.25)} for name,value in a.items()]
    return {'status':'regression' if any(x['regression'] for x in changes) else 'passed','changes':changes,
            'source_before':before['source'],'source_after':after['source'],'threshold_percent':25,
            'scope':'matching lab preset only; generator and relay costs separate; no baseline promotion or Pi capacity claim'}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('connections',choices=tuple(map(str,COUNTS))+('compare',))
    parser.add_argument('--baseline',type=Path)
    parser.add_argument('--candidate',type=Path)
    parser.add_argument('--run',action='store_true',help='start the isolated fixture; default only prints the plan')
    parser.add_argument('--output',type=Path)
    args=parser.parse_args()
    if args.connections=='compare':
        if args.run or not args.baseline or not args.candidate:parser.error('compare requires --baseline and --candidate, without --run')
        result=compare(json.loads(args.baseline.read_text()),json.loads(args.candidate.read_text()))
        if args.output:
            with args.output.open('x') as stream:json.dump(result,stream,indent=2)
        else:print(json.dumps(result,indent=2))
        return 1 if result['status']=='regression' else 0
    if args.baseline or args.candidate:parser.error('comparison paths require compare mode')
    selected=profile(int(args.connections))
    if not args.run:
        print(json.dumps({'status':'plan_only','stack_started':False,'profile':selected},indent=2))
        return
    if args.output and args.output.exists():
        parser.error('output already exists; preserve prior evidence and select a new path')
    from run import Run
    runner=Run(args.output,accounts=selected['accounts'],load_profile=selected)
    previous={sig:signal.getsignal(sig) for sig in (signal.SIGTERM,signal.SIGINT)}
    def interrupted(*_):
        # Let Run's finally clean its exact project once; another signal must not
        # interrupt that cleanup and leave provider containers/credentials behind.
        for sig in previous:signal.signal(sig,signal.SIG_IGN)
        raise InterruptedError('capacity run interrupted')
    try:
        for sig in previous:signal.signal(sig,interrupted)
        runner.run()
    finally:
        for sig,handler in previous.items():signal.signal(sig,handler)
    print(json.dumps({'status':runner.results['status'],'results':str(runner.output)}))

if __name__=='__main__': raise SystemExit(main())
