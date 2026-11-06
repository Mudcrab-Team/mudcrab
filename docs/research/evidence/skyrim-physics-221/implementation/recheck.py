from pathlib import Path
import hashlib,json,re,struct,subprocess,sys,zipfile
root=Path(__file__).resolve().parents[5]
sys.path.insert(0,str(root/'scripts'))
import check_skyrim_jump_runtime as c
import verify_skyrim_jump_inputs as v
import report_skyrim_jump_runtime as r
sha=lambda value:hashlib.sha256(value).hexdigest()
f32=lambda value:struct.unpack('<f',struct.pack('<f',value))[0]
payload,observations,provenance=c.prepare()
directory=c.EVIDENCE/'implementation'
original_input=(directory/'baseline/input.json').read_bytes()
assert original_input==(directory/'current/input.json').read_bytes()==c.canonical(payload)+b'\n'
assert set(payload)=={'schema','profile','runs'}
assert set(payload['profile'])=={'effective_jump_height_creation'}
assert all(set(run)=={'id','initial_floor_height','controller_dt_seconds'} for run in payload['runs'])
assert len(set(provenance['trace_sha256'].values()))==15
summary={'input_sha256':sha(original_input),'unique_training':sum(run['split']=='train' for run,_ in observations.values()),'unique_heldout':sum(run['split']=='heldout' for run,_ in observations.values()),'replay':{},'headless':{}}
assert summary['unique_training']==3 and summary['unique_heldout']==12
for version in ['baseline','current']:
 folder=directory/version
 result=json.loads((folder/'result.json').read_bytes())
 receipt=json.loads((folder/'source.json').read_bytes())
 output=json.loads((folder/'predictions.json').read_bytes())
 rescored=c.score(output,observations)
 assert result['runs']==rescored
 assert result['rules']==c.RULES
 assert result['input_sha256']==receipt['input_sha256']==sha((folder/'input.json').read_bytes())
 assert result['source_receipt_sha256']==sha((folder/'source.json').read_bytes())
 assert result['prediction_sha256']==sha((folder/'predictions.json').read_bytes())
 assert result['source_sha256']==receipt['source_sha256']==sha(c.canonical(receipt['sources']))
 for key in ['corpora','source_archives','profile_source','trace_sha256']:
  assert receipt[key]==provenance[key]
 heldout=[row for row in rescored if row['split']=='heldout']
 metrics={'n':len(heldout),'passes':sum(row['passes_frozen_report_gate'] for row in heldout),'maximum_rmse':max(row['rmse'] for row in heldout),'maximum_pointwise_error':max(row['max_abs'] for row in heldout)}
 assert metrics==result['heldout']
 summary['replay'][version]=metrics
 if version=='current':
  assert all(sha((root/name).read_bytes())==expected for name,expected in receipt['sources'].items())
 else:
  with zipfile.ZipFile(folder/'production-source.zip') as archive:
   assert set(archive.namelist())==set(receipt['sources'])
   assert all(sha(archive.read(name))==expected for name,expected in receipt['sources'].items())
   old=archive.read('crates/engine/src/physics.rs')
   base=subprocess.check_output(['git','show','1dcca42fc398baaf626e588694f9e08084117f11:crates/engine/src/physics.rs'],cwd=root)
   assert old.replace(b'\n#[cfg(test)]\nmod jump_replay_tests;\n',b'',1)==base
 head=json.loads((folder/'headless.json').read_bytes())
 rest=head['rest_position'][1]; rows=head['samples']
 apex=max(range(len(rows)),key=lambda index:(rows[index]['position'][1],index))
 landing=next(index for index in range(apex+1,len(rows)) if rows[index]['kcc_supported'] and abs(f32(rows[index]['position'][1]-rest))<2)
 assert head['first_delta_y']==f32(rows[0]['position'][1]-rest)
 assert head['sampled_apex_rise']==f32(rows[apex]['position'][1]-rest)
 assert head['apex_tick']==apex+1 and head['landing_tick']==landing+1
 assert head['landing_delta_y']==f32(rows[landing]['position'][1]-rest)
 if version=='current':
  assert head['effective_jump_height_creation']==76
  assert all(row['fixed_dt_seconds']==head['dt_seconds'] for row in rows)
 summary['headless'][version]={key:head[key] for key in ['first_delta_y','sampled_apex_rise','apex_tick','landing_tick']}
corpus=root/'tools/skyrim-recorder/reference/jump-inputs-v4'
verified=v.verify(corpus,root/'tools/skyrim-recorder/fixtures/jump-inputs-v4.json')
assert verified==json.loads((corpus/'result.json').read_bytes())
summary['native_inputs']={'status':verified['status'],'accepted':len(verified['runs']),'attempts':verified['attempts'],'failed':len(verified['failed_attempts']),'archive_sha256':sha((corpus/'capture-source.zip').read_bytes())}
assert all(row['launch_prediction_error']==0 for row in verified['runs'])
chart=r.render()
assert chart==(directory/'comparison.html').read_text()
summary['chart_sha256']=sha(chart.encode())
links={}
for filename in ['docs/research/skyrim-physics-221-implementation.md','docs/player-movement-record-map.md','docs/research/evidence/skyrim-physics-221/implementation/baseline/README.md','tools/skyrim-recorder/reference/jump-inputs-v4/implementation-inputs.md']:
 path=root/filename
 local=[link for link in re.findall(r'\[[^\]]*\]\(([^)]+)\)',path.read_text()) if '://' not in link and not link.startswith('#')]
 assert all((path.parent/link.split('#')[0]).exists() for link in local)
 links[filename]=len(local)
summary['resolving_local_links']=links
print(json.dumps(summary,indent=2))
