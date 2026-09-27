import copy,json,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parent))
import capacity

class CapacityProfiles(unittest.TestCase):
    def test_exact_unique_identity_matrix_and_proportional_thresholds(self):
        for total in capacity.COUNTS:
            p=capacity.profile(total)
            self.assertEqual(p['macs']+p['clients'],total)
            self.assertEqual(p['clients'],2*p['macs']+p['matrix']['extra_client_macs'])
            for route,count in [('work',p['macs']),('changes',p['clients']),('total',total)]:
                self.assertEqual(p['server_waiter_gate']['thresholds'][route],(80*count+99)//100)
            self.assertEqual(p['server_waiter_gate']['steady_fraction_percent'],80)
            self.assertEqual(p['duration_seconds'],600)
            self.assertEqual(p['short_latency_budget_ms'],{'p95':500,'p99':2000})
            self.assertGreater(p['compiled_limits']['requests'],total)
            self.assertEqual(p['auth']['initial_grants'],16)
    def test_profiles_are_independent_and_bounded(self):
        p=capacity.profile(1000);p['server_waiter_gate']['thresholds']['work']=0
        self.assertGreater(capacity.profile(1000)['server_waiter_gate']['thresholds']['work'],0)
        for invalid in (True,999,1001,0,100000,'1000'):
            with self.assertRaises(ValueError):capacity.profile(invalid)
    def test_default_is_plan_only_without_fixture_or_docker(self):
        result=subprocess.run([sys.executable,str(Path(capacity.__file__)),'1000'],capture_output=True,text=True,timeout=3,check=True)
        plan=json.loads(result.stdout)
        self.assertEqual(plan['status'],'plan_only');self.assertFalse(plan['stack_started'])
        self.assertNotIn('runtime',plan)
    def test_capacity_limits_are_isolated_from_baseline_compose(self):
        import run
        calls=[]
        baseline=run.Run(command=lambda args,**kw:calls.append(args))
        selected=run.Run(command=lambda args,**kw:calls.append(args),accounts=16,load_profile=capacity.profile(1000))
        try:
            baseline.compose('config','--quiet')
            selected.compose('config','--quiet')
            self.assertEqual(calls[0].count('--file'),1)
            self.assertEqual(calls[1].count('--file'),2)
            override=json.loads(selected.capacity_override.read_text())
            self.assertEqual(override['services']['client']['ulimits']['nofile']['soft'],65536)
            self.assertFalse('SYSTEM_CAPACITY_PRESET' in baseline.env_file.read_text())
            self.assertEqual(selected.capacity_override.stat().st_mode & 0o777,0o600)
            self.assertNotIn('ulimits:',(Path(capacity.__file__).parent/'compose.yaml').read_text())
        finally:
            shutil.rmtree(baseline.runtime);shutil.rmtree(selected.runtime)

    def test_rejects_edited_profile_before_creating_fixture(self):
        from unittest.mock import patch
        import run
        p=capacity.profile(1000);p['server_waiter_gate']['thresholds']['work']=1
        with patch.object(run.tempfile,'mkdtemp') as create:
            with self.assertRaises(ValueError):run.Run(accounts=16,load_profile=p)
            create.assert_not_called()

    def test_pending_observer_rejects_cohort_threshold_reduction(self):
        import run
        p=capacity.profile(1000)
        thresholds=p['server_waiter_gate']['thresholds']
        sample={'schema':1,'scope':'lab_pending_futures','epoch':'a'*22,'sequence':1,'uptime_ms':1}
        for route,threshold in thresholds.items():
            sample[route]={'current':0,'peak':0,'entered':0,'exited':0,'threshold':threshold,'above_ms':0,'longest_above_ms':0}
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'waiters.json';path.write_text(json.dumps(sample))
            run.waiter_observation(path,thresholds=thresholds)
            for route in thresholds:
                changed=copy.deepcopy(sample);changed[route]['threshold']=200
                path.write_text(json.dumps(changed))
                with self.assertRaises(ValueError):run.waiter_observation(path,thresholds=thresholds)

class GeneratorTermination(unittest.TestCase):
    def test_exec_exit_race_uses_real_terminal_state_not_python_flag(self):
        import run
        calls=[]
        # docker wait has not returned yet, but the process exited between stats
        # and exec. A state check can prove this is a clean terminal sample gap.
        def inspect(args,**kwargs):
            calls.append((args,kwargs))
            return json.dumps({'running':False,'oom_killed':False,'exit_code':0})
        event,failed=run.generator_sample_failure(inspect,'owned-load-client',RuntimeError())
        self.assertFalse(failed)
        self.assertEqual(event['classification'],'clean_terminal_between_samples')
        self.assertEqual(calls[0][0][-1],'owned-load-client')
        self.assertEqual(calls[0][1]['timeout'],4)
    def test_does_not_hide_running_oom_nonzero_or_unobservable_errors(self):
        import run
        for state in ({'running':True,'oom_killed':False,'exit_code':0},
                      {'running':False,'oom_killed':True,'exit_code':0},
                      {'running':False,'oom_killed':False,'exit_code':137},{}):
            _,failed=run.generator_sample_failure(lambda *a,**k:json.dumps(state),'owned',RuntimeError())
            self.assertTrue(failed)
        def unavailable(*args,**kwargs):raise TimeoutError()
        event,failed=run.generator_sample_failure(unavailable,'owned',RuntimeError())
        self.assertTrue(failed);self.assertEqual(event['state_error'],'TimeoutError')
    def test_join_timeout_preserves_generator_and_marks_incomplete_evidence(self):
        from unittest.mock import Mock,patch
        import run
        calls=[]
        def command(args,**kw):
            calls.append(args)
            return '0' if args[1]=='wait' else ''
        runner=run.Run(command=command);runner.load_profile={'duration_seconds':30}
        observer=Mock();observer.is_alive.return_value=True
        def compose(*args,**kwargs):return 'a'*64 if args[0]=='ps' else ''
        try:
            with patch.object(run.threading,'Thread',return_value=observer),patch.object(runner,'compose',side_effect=compose),patch.object(run,'IDLE_OBSERVATION_SECONDS',0):
                with self.assertRaisesRegex(RuntimeError,'resource sampler'):runner.load()
            observer.join.assert_called_once_with(timeout=run.SAMPLER_JOIN_SECONDS)
            self.assertFalse(any(args[1]=='rm' for args in calls))
            self.assertFalse(runner.results['resources']['sampler_joined'])
            with self.assertRaisesRegex(RuntimeError,'sampler still active'):runner.cleanup()
        finally:shutil.rmtree(runner.runtime)

    def test_live_sampler_blocks_resource_cleanup(self):
        from unittest.mock import Mock
        import run
        calls=[];runner=run.Run(command=lambda *a,**k:calls.append(a))
        try:
            runner.started=True
            runner._load_sampler=Mock();runner._load_sampler.is_alive.return_value=True
            with self.assertRaisesRegex(RuntimeError,'sampler still active'):runner.cleanup()
            self.assertEqual(calls,[])
            self.assertTrue(runner.started)
        finally:shutil.rmtree(runner.runtime)

class Cancellation(unittest.TestCase):
    def test_signal_enters_owned_cleanup_and_repeat_signal_is_ignored(self):
        from unittest.mock import patch
        import run,signal
        observed=[]
        class Runner:
            def __init__(self,*args,**kwargs):pass
            def run(self):
                try:
                    signal.getsignal(signal.SIGTERM)(signal.SIGTERM,None)
                finally:
                    observed.append(signal.getsignal(signal.SIGTERM)==signal.SIG_IGN)
                    observed.append('owned cleanup reached')
        before=signal.getsignal(signal.SIGTERM)
        with patch.object(run,'Run',Runner),patch.object(sys,'argv',['capacity.py','1000','--run']):
            with self.assertRaises(InterruptedError):capacity.main()
        self.assertEqual(observed,[True,'owned cleanup reached'])
        self.assertEqual(signal.getsignal(signal.SIGTERM),before)

class Comparison(unittest.TestCase):
    def evidence(self):
        p=capacity.profile(1000)
        row={'phase':'steady','process':{'VmRSS':'100000 kB'},'open_fds':100,'cpu_percent':'20%'}
        return {'status':'passed','source':{'commit':'a'*40,'dirty':False},
                'hardware':{'docker':{'cpus':4}},'versions':{'rust':'synthetic'},
                'instrumentation':{'feature':'lab-waiter-metrics'},'load_configuration':p,
                'cases':[{'phase':'capacity','status':'passed','steady_passed':True,'recovery_passed':True,'short_latency':{'passed':True},
                          'bursts':{'p95_ms':20,'p99_ms':40},'recovery':{'elapsed_ms':10000}}],
                'resources':{'limits':p['resource_limits'],'sample_errors':0,'sampler_joined':True,
                             'client_terminal_state':{'running':False,'oom_killed':False,'exit_code':0},
                             'observations':[row,{**row,'phase':'idle_before_restart'}],
                             'client_observations':[row]}}
    def test_like_for_like_detects_regression_without_promoting_baseline(self):
        before=self.evidence();after=copy.deepcopy(before)
        self.assertEqual(capacity.compare(before,after)['status'],'passed')
        after['cases'][0]['bursts']['p99_ms']=100
        result=capacity.compare(before,after)
        self.assertEqual(result['status'],'regression')
        self.assertEqual([x['metric'] for x in result['changes'] if x['regression']],['short_p99_ms'])
        self.assertEqual(before['cases'][0]['bursts']['p99_ms'],40)
    def test_rejects_incomplete_or_false_green_resource_evidence(self):
        for mutate in (
            lambda r:r.update(status='failed'),
            lambda r:r['cases'][0].update(steady_passed=False),
            lambda r:r['cases'][0].update(recovery_passed=False),
            lambda r:r['cases'][0]['short_latency'].update(passed=False),
            lambda r:r['source'].update(dirty=True),
            lambda r:r['resources'].update(sample_errors=1),
            lambda r:r['resources']['client_terminal_state'].update(oom_killed=True),
            lambda r:r['resources'].update(client_observations=[]),
            lambda r:r['resources']['client_observations'][0].update(cpu_percent='NaN%'),
            lambda r:r['load_configuration'].update(duration_seconds=30),
        ):
            after=copy.deepcopy(self.evidence());mutate(after)
            with self.assertRaises(ValueError):capacity.compare(self.evidence(),after)
    def test_rejects_different_environment_and_resource_budget(self):
        for field in ('hardware','versions','instrumentation'):
            after=self.evidence();after[field]={'different':True}
            with self.assertRaises(ValueError):capacity.compare(self.evidence(),after)
        after=self.evidence();after['resources']['limits']=dict(after['resources']['limits'],relay_cpus=8)
        with self.assertRaises(ValueError):capacity.compare(self.evidence(),after)

if __name__=='__main__':unittest.main()

