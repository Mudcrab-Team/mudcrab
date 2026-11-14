"""Pure/synthetic actor oracle and mocked controller wiring; no native/OS control."""
import copy
import importlib.util
import json
import re
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import MagicMock, patch

import actor_idle_oracle as oracle
import actor_idle_tools
from actor_idle_tools import load_json


class _Refused(ValueError):
    pass


def _controller_controls():
    """Supply local controls so portable wiring tests need no private evidence tree."""
    def check_sample(_elapsed, memory, _log_bytes):
        if memory['rss'] >= 2 * 1024**3 or memory['footprint'] >= 2 * 1024**3:
            raise _Refused('injected memory ceiling')

    control = SimpleNamespace(
        Refused=_Refused, Darwin=MagicMock(), GAME=re.compile(r'(?!)'),
        FAULT=re.compile(rb'validation error'),
        check_host=MagicMock(), check_sample=check_sample, command=MagicMock(),
    )
    return {
        'geometry_watchdog': control,
        'materials_process': SimpleNamespace(processes=MagicMock()),
        'lifecycle': SimpleNamespace(OwnedChild=MagicMock()),
        'lease_admission': SimpleNamespace(held_build_leases=MagicMock()),
    }


# The production loader keeps its exact pins. These tests exercise the actor
# adapter with local doubles and do not import or qualify operational helpers.
_shared = actor_idle_tools.shared
try:
    actor_idle_tools.shared = lambda name: _controller_controls()[name]
    _spec = importlib.util.spec_from_file_location(
        '_test_actor_idle_watchdog_' + uuid.uuid4().hex,
        Path(__file__).with_name('actor_idle_watchdog.py'))
    watchdog = importlib.util.module_from_spec(_spec)
    _spec.loader.exec_module(watchdog)
finally:
    actor_idle_tools.shared = _shared


def fixture():
    contract = {'self_sha256': 'c'*64, 'dimensions': [8, 8], 'settle_frames': 10,
                'maximum_sample_vertices': 4096, 'vertex_motion_epsilon': 1e-6,
                'minimum_moving_vertices': 1, 'background_max_u8': 2,
                'foreground_min_u8': 32, 'minimum_foreground_pixels': 2,
                'pixel_difference_u8': 2, 'minimum_changed_pixels': 1,
                'limits': ['Synthetic data; no native result.']}
    case = {'name': 'cow', 'base_form_id': 1, 'race_id': 2, 'sex': 'male',
            'body_scene': 'meshes/cow.glb', 'source': 'meshes/cow/idle.hkx',
            'clip_scene': 'meshes/actor-idles/clips/cow.glb',
            'body_sha256': 'a'*64, 'clip_sha256': 'b'*64, 'sample_seconds': .5,
            'expected_skinned_meshes': 1, 'expected_skin_vertices': 3}
    report = {key: value for key, value in case.items() if key != 'sample_seconds'}
    report.update(version=1, scope='actor-only-unlit-skinning',
                  fixture_spawn='catalog-plan-with-explicit-live-idle', actor_drive=False,
                  graph_clip_matches=True, curve_targets=10, unbound_targets=0,
                  skinned_meshes=1, skinned_vertices_total=3, readback_frames=2,
                  contract_sha256='c'*64, adapter={'backend': 'Metal'},
                  poses=[{'seconds': 0, 'settled_frames': 10, 'vertices': [{'id': 'v1', 'position': [0,0,0]}]},
                         {'seconds': .5, 'settled_frames': 10, 'vertices': [{'id': 'v1', 'position': [.1,0,0]}]}])
    for pose in report['poses']:
        pose['camera_matrix'] = [1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]
    def image(offset):
        pixels = bytearray(8*8*3)
        for i in (3*8+2+offset, 4*8+2+offset):
            pixels[3*i:3*i+3] = bytes([255]*3)
        return 8, 8, pixels
    return contract, case, report, [image(0), image(1)]


class Oracle(unittest.TestCase):
    def test_selected_pair_passes_only_synthetic_scope(self):
        result = oracle.evaluate(*fixture())
        self.assertEqual(result['moving_sample_vertices'], 1)
        self.assertEqual(result['changed_pixels'], 4)
        self.assertEqual(result['status'], 'selected-native-skinning-motion-detected-uncalibrated')
        self.assertFalse(result['thresholds_calibrated'])
        self.assertEqual(result['moving_sample_fraction'], 1)
        self.assertEqual(result['changed_pixel_fraction'], 4/64)
        self.assertGreater(result['maximum_displacement_epsilon_ratio'], 1)
        self.assertIn('Synthetic', result['limits'][0])

    def test_wrong_identity_asset_contract_graph_or_backend_rejects(self):
        for key, value in [('name','chicken'), ('base_form_id',7), ('race_id',9), ('sex','female'),
                           ('body_scene','meshes/other.glb'), ('source','meshes/other.hkx'),
                           ('clip_scene','meshes/other.glb'), ('body_sha256','d'*64),
                           ('clip_sha256','d'*64), ('contract_sha256','d'*64),
                           ('graph_clip_matches',False), ('actor_drive',True),
                           ('unbound_targets',1), ('curve_targets',0), ('skinned_meshes',0),
                           ('skinned_vertices_total',0), ('readback_frames',1),
                           ('fixture_spawn','production-spawn'), ('adapter',{'backend':'Vulkan'})]:
            with self.subTest(key=key):
                c,k,r,i = fixture(); r[key] = value
                with self.assertRaises(ValueError): oracle.evaluate(c,k,r,i)

    def test_blank_and_identical_readbacks_cannot_pass_cpu_motion(self):
        for blank in (False, True):
            c,k,r,i = fixture()
            if blank: i = [(8,8,bytearray(8*8*3))]*2
            else: i[1] = i[0]
            with self.assertRaises(ValueError): oracle.evaluate(c,k,r,i)

    def test_changed_pixels_cannot_hide_static_cpu_skin(self):
        c,k,r,i = fixture(); r['poses'][1]['vertices'] = copy.deepcopy(r['poses'][0]['vertices'])
        with self.assertRaisesRegex(ValueError, 'did not deform'): oracle.evaluate(c,k,r,i)

    def test_missing_skin_primitives_or_vertices_cannot_pass_a_partial_body(self):
        for key in ('expected_skinned_meshes', 'expected_skin_vertices'):
            c,k,r,i = fixture(); k[key] += 1
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'denominator'):
                oracle.evaluate(c,k,r,i)

    def test_missing_nonfinite_reordered_or_duplicate_samples_reject(self):
        for change in ('empty','nonfinite','id','duplicate','time','settle'):
            c,k,r,i = fixture()
            if change == 'empty': r['poses'][1]['vertices'] = []
            if change == 'nonfinite': r['poses'][1]['vertices'][0]['position'][0] = float('nan')
            if change == 'id': r['poses'][1]['vertices'][0]['id'] = 'other'
            if change == 'duplicate':
                for p in r['poses']: p['vertices'] *= 2
            if change == 'time': r['poses'][1]['seconds'] = .7
            if change == 'settle': r['poses'][1]['settled_frames'] = 1
            with self.subTest(change=change), self.assertRaises(ValueError): oracle.evaluate(c,k,r,i)

    def test_capture_border_and_dimensions_are_part_of_denominator(self):
        c,k,r,i = fixture(); i[1][2][0:3] = bytes([255]*3)
        with self.assertRaises(ValueError): oracle.evaluate(c,k,r,i)

    def test_camera_motion_cannot_explain_changed_pixels(self):
        c,k,r,i = fixture(); r['poses'][1]['camera_matrix'][12] = .5
        with self.assertRaisesRegex(ValueError, 'camera'): oracle.evaluate(c,k,r,i)
        c,k,r,i = fixture(); i[1] = (7,8,i[1][2])
        with self.assertRaises(ValueError): oracle.evaluate(c,k,r,i)

    def test_json_rejects_duplicate_keys_nan_and_oversize(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'report.json'
            for contents in ('{"a":1,"a":2}', '{"a":NaN}'):
                path.write_text(contents)
                with self.assertRaises(ValueError): load_json(path)
            path.write_text('{}')
            with self.assertRaises(ValueError): load_json(path, limit=1)


def admission_fixture():
    plan = {'version':2, 'runnable':True, 'case_names':['cow','chicken','wolf'],
            'wall_seconds':180, 'per_child_seconds':60,
            'memory_gate':{'status':'admitted','sha256':'a'*64}}
    admission = {'plan_sha256':'p'*64, 'gpu_owner':'animals', 'grant_reference':'named grant',
                 'expires_epoch':200, 'other_gpu_campaigns':0, 'memory_recovery_sha256':'a'*64}
    ledger = {'resourcePolicy':{'gpuOwner':'animals'}}
    return plan, 'p'*64, admission, ledger, 100


class Admission(unittest.TestCase):
    def test_valid_pure_admission(self):
        watchdog.check_admission(*admission_fixture())

    def test_closed_expired_wrong_owner_or_case_denominator_rejects(self):
        mutations = [('plan','runnable',False), ('plan','case_names',['cow','cow']),
                     ('plan','case_names',['../cow']), ('plan','case_names',['a','b','c','d']),
                     ('plan','per_child_seconds',90), ('admission','plan_sha256','wrong'),
                     ('admission','gpu_owner','geometry'), ('admission','grant_reference',''),
                     ('admission','expires_epoch',99), ('admission','expires_epoch',2000),
                     ('admission','other_gpu_campaigns',1), ('admission','memory_recovery_sha256','b'*64)]
        for target,key,value in mutations:
            p,h,a,l,t = admission_fixture(); (p if target=='plan' else a)[key]=value
            with self.subTest(key=key), self.assertRaises(ValueError): watchdog.check_admission(p,h,a,l,t)
        p,h,a,l,t = admission_fixture(); l['resourcePolicy']['gpuOwner']=None
        with self.assertRaises(ValueError): watchdog.check_admission(p,h,a,l,t)

    def test_native_memory_hold_rejects_without_sampling(self):
        p,h,a,l,t = admission_fixture(); p['memory_gate']['status']='blocked'
        with patch.object(watchdog.control,'Darwin') as darwin, patch.object(watchdog.materials,'processes') as processes:
            with self.assertRaisesRegex(ValueError,'memory block'): watchdog.check_admission(p,h,a,l,t)
            darwin.assert_not_called(); processes.assert_not_called()

    def test_lease_snapshot_delegates_to_exact_pinned_helper(self):
        with patch.object(watchdog.lease_control,'held_build_leases',return_value={'owned'}) as held:
            self.assertEqual(watchdog.held_leases(Path('/storage')), {'owned'})
            held.assert_called_once_with(Path('/storage/locks'))

    def test_closed_template_never_constructs_darwin_or_spawns(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); p=root/'plan.json'; a=root/'admission.json'; l=root/'ledger.json'
            plan,h,admission,ledger,t = admission_fixture(); plan.update(runnable=False, ledger=str(l))
            for path, value in ((p,plan),(a,admission),(l,ledger)): path.write_text(json.dumps(value))
            with patch.object(watchdog.sys,'argv',['watchdog','--plan',str(p),'--admission',str(a)]), \
                 patch.object(watchdog.control,'Darwin') as darwin, patch.object(watchdog.subprocess,'Popen') as popen:
                with self.assertRaisesRegex(ValueError,'closed'): watchdog.main()
                darwin.assert_not_called(); popen.assert_not_called()


class HeldForIntervention(RuntimeError):
    pass


class FakeOwnership:
    """Adapter seam only. This does not reimplement or qualify the shared lifecycle."""
    def __init__(self, child, cleanup_error=False, ambiguous_spawn=False, extra_child=False):
        self.next_child=child; self.child=None; self.attempted=False; self.known={}; self.calls=[]
        self.cleanup_error=cleanup_error; self.ambiguous_spawn=ambiguous_spawn; self.extra_child=extra_child

    def install(self): self.calls.append('install')
    def arm(self, seconds): self.calls.append(('arm',seconds))
    def spawn(self, argv, **kwargs):
        self.calls.append(('spawn',argv,kwargs)); self.attempted=True
        if self.ambiguous_spawn: raise RuntimeError('no Popen handle returned')
        self.child=self.next_child; self.known[self.child.pid]='birth'; return self.child
    def observe(self):
        family={self.child.pid}
        if self.extra_child: family.add(self.child.pid+1)
        return {},family
    def mask_cleanup(self): self.calls.append('mask_cleanup')
    def cleanup(self):
        self.calls.append('cleanup')
        if self.cleanup_error: raise RuntimeError('injected cleanup failure')
    def restore(self): self.calls.append('restore')
    def description(self): return {'pid':self.child.pid if self.child is not None else None,'known':self.known}
    def hold(self, callback):
        self.calls.append('hold')
        try: callback()
        except BaseException: pass
        raise HeldForIntervention('passive foreground hold requested')


class ControllerWiring(unittest.TestCase):
    def run_mock(self, tmp, fault=False, pressure=False, cleanup_error=False,
                 ambiguous_spawn=False, extra_child=False, revoke=False, receipt_error=False):
        child=MagicMock(pid=912345,returncode=0); child.poll.side_effect=[None,0]
        child.stdout.fileno.return_value=99
        owned=FakeOwnership(child,cleanup_error,ambiguous_spawn,extra_child)
        backend=MagicMock(); backend.host.return_value={'available':16*watchdog.GIB,'swap':0}
        backend.memory.return_value={'rss':(2*watchdog.GIB if pressure else 1024),'footprint':1024}
        selector=MagicMock(); selector.select.return_value=[('read',None)]
        data=[b'validation error' if fault else b'native readback',b'',b'']
        def guard(owner):
            if revoke: raise ValueError('revoked before spawn')
            return {}
        with patch.object(watchdog.lifecycle,'OwnedChild',return_value=owned), \
             patch.object(watchdog.os,'set_blocking'), patch.object(watchdog.os,'read',side_effect=data), \
             patch.object(watchdog.selectors,'DefaultSelector',return_value=selector), \
             patch.object(watchdog.time,'sleep'):
            run=Path(tmp)/'cow'
            plan={'assets_root':'/readonly/assets','contract':'/frozen/contract.json','contract_sha256':'c'*64}
            def invoke():
                watchdog.run_case(Path('/retained/actor_idle_probe'),Path(tmp),'cow',run,plan,backend,guard,time.monotonic()+180)
            if receipt_error:
                with patch.object(watchdog,'save',side_effect=OSError('receipt store')):
                    with self.assertRaises(HeldForIntervention): invoke()
            elif cleanup_error or ambiguous_spawn:
                with self.assertRaises(HeldForIntervention): invoke()
            elif fault or pressure or extra_child or revoke:
                with self.assertRaises((ValueError,watchdog.control.Refused)): invoke()
            else: invoke()
            if ambiguous_spawn:
                self.assertNotIn('cleanup',owned.calls)  # No identity/handle: do not guess a PID.
            else:
                self.assertEqual(owned.calls.count('cleanup'),1)
            if cleanup_error or ambiguous_spawn or receipt_error:
                self.assertEqual(owned.calls.count('hold'),1); self.assertNotIn('restore',owned.calls)
            else:
                self.assertEqual(owned.calls.count('restore'),1)
                report=json.loads((run/'control.json').read_text())
                self.assertEqual(report['status'],'failed' if fault or pressure or extra_child or revoke else 'readback-pending-oracle')
            if not revoke:
                command=next(call for call in owned.calls if isinstance(call,tuple) and call[0]=='spawn')
                self.assertTrue(command[2]['start_new_session'])
                self.assertEqual(command[2]['env']['MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256'],'c'*64)
                self.assertEqual(command[1][:3],['/retained/actor_idle_probe','--assets','/readonly/assets'])
            else: self.assertFalse(owned.attempted)
            self.assertEqual(owned.calls[0],'install')
            self.assertEqual(owned.calls[1][0],'arm')
            self.assertLessEqual(owned.calls[1][1],60)
            return owned

    def test_success_uses_shared_ownership_seam_and_still_requires_oracle(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp)
    def test_renderer_fault_cleans_once_and_preserves_receipt(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,fault=True)
    def test_combined_controller_child_memory_is_capped(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,pressure=True)
    def test_pre_spawn_revocation_creates_no_child(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,revoke=True)
    def test_unexpected_descendant_stops_at_actor_policy(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,extra_child=True)
    def test_cleanup_failure_holds_without_retry_or_restore(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,cleanup_error=True)
    def test_ambiguous_popen_result_holds_without_pid_guess(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,ambiguous_spawn=True)
    def test_receipt_failure_after_cleanup_holds_without_repeated_signals(self):
        with tempfile.TemporaryDirectory() as tmp: self.run_mock(tmp,receipt_error=True)
    def test_existing_case_directory_rejects_without_ownership_installation(self):
        with tempfile.TemporaryDirectory() as tmp:
            run=Path(tmp)/'cow'; run.mkdir()
            with patch.object(watchdog.lifecycle,'OwnedChild') as constructor:
                with self.assertRaises(FileExistsError):
                    watchdog.run_case(Path('/binary'),Path(tmp),'cow',run,{},MagicMock(),None,time.monotonic()+180)
                constructor.assert_not_called()


class CompiledIdentity(unittest.TestCase):
    def test_python_successor_does_not_require_rust_rebuild(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); (root/'input.rs').write_text('// Rust stays unchanged')
            (root/'adapter.py').write_text('# initial Python')
            manifest={'head':'frozen-head','checkout':str(root),'files':{'input.rs':watchdog.sha(root/'input.rs')}}
            path=root/'compiled.json';path.write_text(json.dumps(manifest))
            plan={'checkout':str(root),'head':'frozen-head','compiled_inputs':{'path':str(path),'sha256':watchdog.sha(path)}}
            def command(args, cwd):
                return 'frozen-head' if 'rev-parse' in args else 'input.rs\0adapter.py\0'
            with patch.object(watchdog.control,'command',side_effect=command):
                self.assertEqual(watchdog.verify_compiled(plan),root)
                (root/'adapter.py').write_text('# later Python identity')
                self.assertEqual(watchdog.verify_compiled(plan),root)
                (root/'input.rs').write_text('// actual Rust mutation')
                with self.assertRaisesRegex(ValueError,'compiled input changed'): watchdog.verify_compiled(plan)

    def test_new_unrecorded_rust_input_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);manifest={'head':'head','checkout':str(root),'files':{}}
            path=root/'compiled.json';path.write_text(json.dumps(manifest))
            plan={'checkout':str(root),'head':'head','compiled_inputs':{'path':str(path),'sha256':watchdog.sha(path)}}
            with patch.object(watchdog.control,'command',side_effect=['head','new.rs\0adapter.py\0']):
                with self.assertRaisesRegex(ValueError,'inventory'): watchdog.verify_compiled(plan)


if __name__ == '__main__':
    unittest.main()
