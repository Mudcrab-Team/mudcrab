"""Source/mock checks only. No helper main, actual process, sampler or signal operation."""
from argparse import Namespace
import json
import hashlib
from pathlib import Path
import re
import signal
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

import actor_idle_cpu_helper as helper
import actor_idle_cpu_qualification as driver


def authorization_fixture():
    plan={'version':1,'cases':['a1','a2'],'bounds':dict(driver.BOUNDS),'prepared_epoch':100}
    auth={'authorized':True,'action':'animals_actor_adapter_cpu_qualification',
          'plan_sha256':'a'*64,'qualification_manifest_sha256':'b'*64,'grant_reference':'animals-cpu-exact',
          'expires_epoch':500,'host_recovery':{'meaningful_change':'explicit recovery','reference':'receipt','confirmed_epoch':150}}
    ledger={'resourcePolicy':{'cpuControlGrant':{'executionAuthorized':True,'owner':'animals',
              'name':'animals-cpu-exact','planSha256':'a'*64},'gpuOwner':None,
              'maxManagedCommands':2,'buildBudgetGiB':60,'reserveGiB':20}}
    return plan,'a'*64,'b'*64,auth,ledger,200


class Authorization(unittest.TestCase):
    def test_exact_authorization_and_recovery_are_required(self):
        driver.authorize(*authorization_fixture())
        for field,value in [('authorized',False),('plan_sha256','wrong'),('grant_reference',''),('expires_epoch',250)]:
            p,h,m,a,l,t=authorization_fixture();a[field]=value
            with self.subTest(field=field),self.assertRaises(driver.Refused):driver.authorize(p,h,m,a,l,t)

    def test_recovery_is_not_inferred_from_an_old_or_empty_receipt(self):
        for field,value in [('confirmed_epoch',99),('reference',''),('meaningful_change','')]:
            p,h,m,a,l,t=authorization_fixture();a['host_recovery'][field]=value
            with self.subTest(field=field),self.assertRaises(driver.Refused):driver.authorize(p,h,m,a,l,t)

    def test_case_count_order_caps_and_live_grant_cannot_expand(self):
        for cases in (['a1'],['a2','a1'],['a1','a2','shared-case']):
            p,h,m,a,l,t=authorization_fixture();p['cases']=cases
            with self.assertRaises(driver.Refused):driver.authorize(p,h,m,a,l,t)
        p,h,m,a,l,t=authorization_fixture();p['bounds']['helper_rss_bytes']*=2
        with self.assertRaises(driver.Refused):driver.authorize(p,h,m,a,l,t)
        p,h,m,a,l,t=authorization_fixture();l['resourcePolicy']['cpuControlGrant']['executionAuthorized']=False
        with self.assertRaises(driver.Refused):driver.authorize(p,h,m,a,l,t)

    def test_closed_execute_refuses_before_import_sampling_signals_or_output(self):
        p,h,m,a,l,t=authorization_fixture();a['authorized']=False;p['ledger']='/unused/ledger'
        args=Namespace(plan=Path('/plan'),manifest=Path('/manifest'),authorization=Path('/authorization'))
        with patch.object(driver,'read_json',side_effect=[p,{},a,l]), \
             patch.object(driver,'sha',return_value='a'*64),patch.object(driver,'load_adapter') as load, \
             patch.object(driver.signal,'signal') as handler,patch.object(driver.signal,'setitimer') as timer, \
             patch.object(driver.Path,'mkdir') as mkdir:
            with self.assertRaisesRegex(driver.Refused,'closed'):driver.execute(args)
            load.assert_not_called();handler.assert_not_called();timer.assert_not_called();mkdir.assert_not_called()


class CpuPolicyChecks(unittest.TestCase):
    def test_cpu_floors_use_actual_values_without_relaxing_native_context(self):
        adapter=SimpleNamespace(control=SimpleNamespace(FAULT=re.compile(b'fault')));old=adapter.control
        with self.assertRaises(ValueError):
            with driver.cpu_context(adapter):
                self.assertIsInstance(adapter.control,driver.CpuPolicy)
                adapter.control.check_host({'available':4*driver.GIB,'swap':0},0,admission=True)
                with self.assertRaises(driver.Refused):adapter.control.check_host({'available':3*driver.GIB,'swap':0},0,admission=True)
                raise ValueError('case failure')
        self.assertIs(adapter.control,old)

    def test_cpu_caps_and_campaign_swap_cover_both_cases(self):
        with self.assertRaises(driver.Refused):driver.CpuPolicy.check_sample(15,{'rss':1,'footprint':1},1)
        with self.assertRaises(driver.Refused):driver.CpuPolicy.check_sample(1,{'rss':320*driver.MIB,'footprint':1},1)
        actual=MagicMock();actual.host.side_effect=[{'available':5*driver.GIB,'swap':0},
                                                   {'available':5*driver.GIB,'swap':256*driver.MIB}]
        sampler=driver.CpuSampler(actual)
        self.assertEqual(sampler.host()['available'],5*driver.GIB)
        with self.assertRaises(driver.Refused):sampler.host()

    def test_sampler_returns_measured_values_and_rejects_helper_oversize(self):
        actual=MagicMock();actual.memory.return_value={'rss':10*driver.MIB,'footprint':8*driver.MIB}
        sampler=driver.CpuSampler(actual)
        self.assertEqual(sampler.memory(912345),dict(actual.memory.return_value, pid=912345))
        actual.memory.return_value={'rss':64*driver.MIB,'footprint':1}
        with self.assertRaises(driver.Refused):sampler.memory(912345)


class Protocol(unittest.TestCase):
    def test_ack_rejects_wrong_digest_pid_and_incomplete_sample(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp)
            path=directory/'samples.jsonl'
            sample={'child':{'pid':123,'rss':1},'controller':{'pid':driver.os.getpid(),'rss':1}}
            raw=json.dumps(sample).encode()
            path.write_bytes(raw)
            self.assertIsNone(driver.first_sample(directory,123))
            path.write_bytes(raw+b'\n')
            digest=driver.first_sample(directory,123)
            self.assertEqual(digest,hashlib.sha256(raw).hexdigest())
            ack={'case':'a1','pid':123,'sample_sha256':digest}
            helper.verify_sample_ack(directory,ack,123,driver.os.getpid())
            for field,value in [('pid',124),('sample_sha256','wrong')]:
                wrong=dict(ack,**{field:value})
                with self.assertRaises(ValueError):helper.verify_sample_ack(directory,wrong,123,driver.os.getpid())
            with self.assertRaises(ValueError):helper.verify_sample_ack(directory,ack,123,456)
            with self.assertRaises(driver.Refused):driver.first_sample(directory,124)
            sample['child']['pid']=124
            raw=json.dumps(sample).encode();path.write_bytes(raw+b'\n')
            ack['sample_sha256']=hashlib.sha256(raw).hexdigest()
            with self.assertRaises(ValueError):helper.verify_sample_ack(directory,ack,123,driver.os.getpid())

    def test_task_output_cap_and_symlink_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'data').write_bytes(b'abc')
            self.assertEqual(driver.output_bytes(root),3)
            (root/'link').symlink_to(root/'data')
            with self.assertRaisesRegex(driver.Refused,'symlink'):driver.output_bytes(root)
            (root/'link').unlink()
            with patch.dict(driver.BOUNDS,{'output_bytes':2}):
                with self.assertRaisesRegex(driver.Refused,'cap'):driver.output_bytes(root)

    def test_helper_protocol_is_metadata_only_and_rejects_wrong_environment(self):
        args=Namespace(case='a1',assets='/not-opened/no-assets',contract='/contract',output='/output')
        env={'WGPU_BACKEND':'metal','RUST_LOG':'info','MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256':'c'*64}
        record=helper.protocol_record(args,123,env,'c'*64)
        self.assertFalse(record['assets_opened']);self.assertEqual(record['payload_bytes'],driver.MIB)
        env['WGPU_BACKEND']='other'
        with self.assertRaises(ValueError):helper.protocol_record(args,123,env,'c'*64)

    def run_case(self,tmp,name,wrong_protocol=False,unexpected=False,survivor=False):
        directory=Path(tmp)/name
        plan={'helper':'/source/helper.py','checkout':tmp,'assets_root':'/sentinel/no-assets',
              'contract':'/source/contract','contract_sha256':'c'*64,'interpreter':sys.executable}
        owner=SimpleNamespace(child=SimpleNamespace(pid=123,returncode=0 if name=='a1' else -signal.SIGTERM),
                              known={123:'birth'},cleanup_attempted=False)
        owner.observe=lambda:({}, {123} if survivor else set())
        owner.description=lambda:{'pid':123,'known':{'123':'birth'},'cleanup_attempted':owner.cleanup_attempted}
        def hold(callback):
            callback();raise driver.Refused('mock foreground hold')
        owner.hold=hold
        adapter=SimpleNamespace(control=SimpleNamespace(FAULT=re.compile(b'fault')))
        native=adapter.control
        def fake_run(binary,repo,case,path,settings,backend,guard,deadline):
            self.assertIsInstance(adapter.control,driver.CpuPolicy)
            path.mkdir();guard(None);guard(owner)
            args=Namespace(case=case,assets=settings['assets_root'],contract=settings['contract'],output=str(path))
            env={'WGPU_BACKEND':'metal','RUST_LOG':'info','MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256':'c'*64}
            ready=helper.protocol_record(args,123,env,'c'*64)
            if wrong_protocol:ready['assets_opened']=True
            (path/'helper-ready.json').write_text(json.dumps(ready))
            sample={'child':{'pid':123,'rss':1,'footprint':1},'controller':{'pid':driver.os.getpid(),'rss':1,'footprint':1}}
            (path/'samples.jsonl').write_text(json.dumps(sample)+'\n')
            (path/'engine.log').write_text('ACTOR_CPU_READY '+case+'\n')
            failure=None
            try:
                if unexpected:raise ValueError('unexpected adapter error')
                guard(owner)
                if case=='a1':
                    helper.verify_sample_ack(path,json.loads((path/'sample-ack.json').read_text()),123,driver.os.getpid())
            except BaseException as exc:
                failure=exc
            owner.cleanup_attempted=True
            receipt={'status':'failed' if failure else 'readback-pending-oracle','samples':1,
                     'command':[str(binary)]+ready['argv'],'exit_code':owner.child.returncode,
                     'ownership':{'pid':123,'known':{'123':'birth'},'cleanup_attempted':True,'spawning':False}}
            if failure:receipt['reason']=str(failure)
            (path/'control.json').write_text(json.dumps(receipt))
            if failure:raise failure
        adapter.run_case=fake_run
        try:
            result=driver.run_binding_case(adapter,MagicMock(),plan,name,directory,lambda owned:None,time.monotonic()+90)
            if name=='a1':self.assertTrue((directory/'sample-ack.json').is_file())
            self.assertFalse(result['native_oracle_called'])
            return result
        finally:
            self.assertIs(adapter.control,native)

    def test_a1_actual_driver_binding_and_sample_ack_with_mock_runner(self):
        with tempfile.TemporaryDirectory() as tmp:
            result=self.run_case(tmp,'a1');self.assertEqual(result['adapter_outcome'],'readback-pending-oracle')

    def test_a2_expected_adapter_failure_is_qualified_without_native_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            result=self.run_case(tmp,'a2');self.assertTrue(result['expected_failure']);self.assertEqual(result['exit_code'],-15)

    def test_wrong_helper_protocol_and_unexpected_error_do_not_pass(self):
        for option in ('wrong_protocol','unexpected'):
            with tempfile.TemporaryDirectory() as tmp,self.assertRaises((driver.Refused,ValueError)):
                self.run_case(tmp,'a1',**{option:True})

    def test_post_case_survivor_requests_hold_without_cleanup_retry(self):
        with tempfile.TemporaryDirectory() as tmp,self.assertRaisesRegex(driver.Refused,'foreground hold'):
            self.run_case(tmp,'a1',survivor=True)

    def test_sequence_stops_first_unexpected_failure_and_never_runs_shared_cases(self):
        calls=[]
        def fail(name):calls.append(name);raise ValueError('first failure')
        with self.assertRaises(ValueError):driver.two_cases(fail,lambda values:None)
        self.assertEqual(calls,['a1'])
        calls=[]
        def succeed(name):
            calls.append(name);return {'id':name.upper(),'status':'passed_cpu_process_protocol_only'}
        values=driver.two_cases(succeed,lambda values:None)
        self.assertEqual(calls,['a1','a2']);self.assertEqual(len(values),2)


if __name__ == '__main__':
    unittest.main()
