import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import evaluate_skyrim_jump as e


def synthetic(no_jump=False):
    rows=[]; inputs=0; qpc=1000000
    def emit(kind,sample=0,code=0,state=0,v=None,label=''):
        nonlocal qpc,inputs
        qpc+=1
        if kind==2: inputs+=1
        rows.append(e.WIRE.pack(len(rows)+1,qpc,sample,inputs,kind,1,code,state,
                                *(v or [0.]*40),label.encode().ljust(48,b'\0')))
    emit(1,1000000,3,label='mudcrab-motion-256-v3')
    dt=1/60; launch=340.; g=900.
    for n in range(1,151):
        qpc=1000000+round(n*dt*1000000)
        if n==20:
            emit(2,n-1,v=[1.,0.]+[0.]*38,label='Jump')
            v=[0.]*40;v[2]=v[20]=dt;v[6]=launch/70;v[16]=v[34]=2;v[36]=70;v[37]=1.35;v[39]=-g/70
            emit(8,n-1,3,v=v);v[16]=1;emit(9,n-1,3,v=v)
        if n==23: emit(2,n-1,v=[0.,.05]+[0.]*38,label='Jump')
        t=(n-19)*dt
        y=max(0,launch*t-g*t*t/2) if n>=20 and not no_jump else 0
        v=[0.]*40;v[2]=dt;v[3]=60;emit(7,n,1,v=v)
        v=[0.]*40;v[17]=v[32]=dt;emit(6,n,3,v=v)
        v=[0.]*40;v[2]=y;emit(3,n,1,v=v)
    emit(5,150)
    return b''.join(rows)


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name)
        self.raw=synthetic();(self.root/'a.bin').write_bytes(self.raw)
        self.run={'id':'a','split':'train','path':'a.bin','sha256':e.digest(self.raw),'frame_cap':60,'time_multiplier':1}
    def tearDown(self):self.temp.cleanup()
    def test_known_recovery_and_forecast(self):
        j=e.qualify(self.root/'a.bin',self.run);m=e.fit([j],'camera')[0]
        self.assertAlmostEqual(m['g'],900,places=2)
        # Phase is one camera boundary, and intentionally not adjusted using flight.
        self.assertAlmostEqual(m['v'],340,places=2)
        self.assertAlmostEqual(e.forecast({'law':'ballistic','g':900,'v':340},[.2])[0],50)
    def test_future_height_cannot_enter_predictor(self):
        j=e.qualify(self.root/'a.bin',self.run);m=e.fit([j],'camera')[0]
        times=[s['camera']-j['origin']['camera'] for s in j['samples']]
        before=e.forecast(m,times)
        for s in j['samples']:s['z']+=100000
        self.assertEqual(before,e.forecast(m,times))
    def test_bad_hash(self):
        with self.assertRaisesRegex(ValueError,'hash'):e.decode(self.root/'a.bin','0'*64)
    def test_dropped_and_footer(self):
        for data,reason in [(self.raw[:256]+self.raw[512:],'sequence'),(self.raw[:-256],'footer'),(self.raw[:-1],'truncated')]:
            (self.root/'bad.bin').write_bytes(data)
            with self.assertRaisesRegex(ValueError,reason):e.decode(self.root/'bad.bin',e.digest(data))
    def test_no_jump(self):
        data=synthetic(True);(self.root/'bad.bin').write_bytes(data)
        with self.assertRaisesRegex(ValueError,'no clear jump'):e.qualify(self.root/'bad.bin',dict(self.run,sha256=e.digest(data)))
    def test_freeze_never_reads_heldout_and_overlap_rejected(self):
        m={'schema':1,'runtime_sha256':e.RUNTIME,'runs':[self.run,dict(self.run,id='unread',split='heldout',path='does-not-exist',sha256='b'*64)]}
        path=self.root/'m.json';path.write_text(json.dumps(m));f=self.root/'f.json'
        e.freeze(path,f)
        m['runs']=[dict(self.run,split='heldout')];path.write_text(json.dumps(m))
        with self.assertRaisesRegex(ValueError,'overlap'):e.score(path,f,self.root/'out')
    def test_score_cannot_fit_heldout(self):
        path=self.root/'m.json';f=self.root/'f.json'
        m={'schema':1,'runtime_sha256':e.RUNTIME,'runs':[self.run]}
        path.write_text(json.dumps(m));e.freeze(path,f)
        rows=[]
        for raw in e.WIRE.iter_unpack(self.raw):
            row=list(raw);row[1]+=100;rows.append(e.WIRE.pack(*row))
        held=b''.join(rows);(self.root/'held.bin').write_bytes(held)
        m['runs']=[dict(self.run,id='held',split='heldout',path='held.bin',sha256=e.digest(held))]
        path.write_text(json.dumps(m))
        with mock.patch.object(e,'fit',side_effect=AssertionError('heldout fit called')):
            result=e.score(path,f,self.root/'out')
        self.assertNotIn('rejected',result['runs'][0])
        self.assertEqual(len(result['runs'][0]['models']),7)
    def test_seal_rejects_changed_model(self):
        path=self.root/'m.json';path.write_text(json.dumps({'schema':1,'runtime_sha256':e.RUNTIME,'runs':[self.run]}));f=self.root/'f.json'
        frozen=e.freeze(path,f);frozen['models'][0]['g']=1;f.write_text(json.dumps(frozen))
        with self.assertRaisesRegex(ValueError,'seal'):e.score(path,f,self.root/'out')
    def test_wrong_clock_remains_wrong(self):
        m={'law':'ballistic','g':900,'v':340}
        game=[i/120 for i in range(60)];wall=[t*2 for t in game]
        right=e.forecast(m,game);wrong=e.forecast(m,wall)
        self.assertGreater(e.metrics([a-b for a,b in zip(right,wrong)])['rmse'],20)
    def test_native_recurrence_has_no_observation_channel(self):
        model={'g':700.,'v':330.,'fall_multiplier':1.35}
        dt=1/60
        trajectory=e.native_forecast(model,[None]+[dt]*100)
        self.assertEqual(trajectory[0],0)
        self.assertAlmostEqual(trajectory[1],(330-700*1.35*dt)*dt)
        self.assertEqual(trajectory[-1],0)
        self.assertGreater(max(trajectory),70)
    def test_native_against_piecewise_closed_form(self):
        dt, g, v, fall = 1/60, 700., 330., 1.35
        model = {'g':g, 'v':v, 'fall_multiplier':fall}
        actual = e.native_forecast(model, [dt]*80)
        def rising(n):
            return n*v*dt - g*dt*dt*(n*(n+1)/2 + .35*n)
        # This fixture first loses height at tick 28; tick 29 takes the history gate.
        velocity28 = v-g*dt*(28+.35)
        expected = [max(0., rising(n) if n<=28 else
                        rising(28)+(n-28)*velocity28*dt-g*fall*dt*dt*(n-28)*(n-27)/2)
                    for n in range(1,81)]
        for a,b in zip(actual,expected):
            self.assertAlmostEqual(a,b,places=10)
    def test_mudcrab_starts_at_rest_then_full_launch_step(self):
        model = {'law':'mudcrab','g':900,'v':340,'h':1/60}
        self.assertEqual(e.forecast(model,[0])[0],0)
    def test_mudcrab_first_displacement(self):
        m={'law':'mudcrab','g':900,'v':340,'h':1/60}
        self.assertAlmostEqual(e.forecast(m,[1/60])[0],340/60)


if __name__=='__main__':unittest.main()
