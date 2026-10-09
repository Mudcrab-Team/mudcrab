#!/usr/bin/env python3
import argparse,json,collections,statistics,math,sys
from pathlib import Path

def main():
 p=argparse.ArgumentParser(description='Correlate observed reuse of native drawable texture pointers')
 p.add_argument('input',help='Timeline JSON produced by analyze_native.py')
 p.add_argument('--output',required=True,help='Summary JSON destination')
 p.add_argument('--pairs-output',help='Pair JSON destination (defaults beside summary)')
 a=p.parse_args();output=Path(a.output);pairs_output=Path(a.pairs_output) if a.pairs_output else output.with_name(output.stem+'-pairs.json')
 data=json.loads(Path(a.input).read_text());frames={};seen={};pairs=[]
 if not isinstance(data,dict):raise ValueError('Input timeline must be a JSON object')
 if not data.get('acquisitions'):raise ValueError('No acquisitions in input timeline')
 if not data.get('command_buffers'):raise ValueError('No command-buffer timings in input timeline; acquisition-only traces cannot identify final render completion')
 if not data.get('presented'):raise ValueError('No presentedTime callbacks in input timeline')
 for b in data['command_buffers']:
  aid=b['acquisition_id']
  if not aid:continue
  f=frames.setdefault(aid,{})
  if b['cb_name']=='(wgpu internal) Present':f['present_buffer']=b
  elif b['label']=='upscaling':f['render_final']=b
 for p in data['presented']:frames.setdefault(p['acquisition_id'],{})['presentation']=p
 for a in data['acquisitions']:
  begin=a['begin'];end=a['end'];key=end['texture'];aid=end['acquisition_id'];old=seen.get(key);seen[key]=aid
  if not old:continue
  prev=frames.get(old,{})
  if not all(k in prev for k in ('render_final','presentation','present_buffer')):continue
  render_ns=prev['render_final']['gpu_end_s']*1e9;onscreen_ns=prev['presentation']['presented_s']*1e9;gpu_ns=prev['present_buffer']['gpu_end_s']*1e9
  if onscreen_ns<=0:continue
  pairs.append({'acquisition_id':aid,'previous_same_texture_acquisition_id':old,'previous_id_distance':aid-old,'texture':key,'wait_begin_ns':begin['host_ns'],'wait_end_ns':end['end_ns'],'gpu_render_end_ns':render_ns,'gpu_present_buffer_end_ns':gpu_ns,'presented_ns':onscreen_ns,'presentation_callback_ns':prev['presentation']['host_ns'],'wait_ms':(end['end_ns']-begin['host_ns'])/1e6,'render_end_to_reacquire_ms':(end['end_ns']-render_ns)/1e6,'onscreen_to_reacquire_ms':(end['end_ns']-onscreen_ns)/1e6,'render_end_to_onscreen_ms':(onscreen_ns-render_ns)/1e6,'wait_before_render_end_ms':max(0,min(end['end_ns'],render_ns)-begin['host_ns'])/1e6,'wait_after_render_end_ms':max(0,end['end_ns']-max(begin['host_ns'],render_ns))/1e6,'wait_after_onscreen_ms':max(0,end['end_ns']-max(begin['host_ns'],onscreen_ns))/1e6})
 if not pairs:raise ValueError('No complete same-texture reuse pairs with upscaling, Present buffer, and presentedTime records')
 if sum(p['wait_ms'] for p in pairs)<=0:raise ValueError('No positive aggregate acquisition wait for timing fractions')
 def stats(vals):
  vals=sorted(vals);return {'n':len(vals),'mean':statistics.mean(vals),'p50':statistics.median(vals),'p95':vals[math.ceil(.95*len(vals))-1],'min':min(vals),'max':max(vals)}
 summary={'pairs':len(pairs),'previous_id_distances':dict(collections.Counter(p['previous_id_distance'] for p in pairs)),'statistics':{k:stats([p[k] for p in pairs]) for k in ('wait_ms','render_end_to_reacquire_ms','onscreen_to_reacquire_ms','render_end_to_onscreen_ms','wait_before_render_end_ms','wait_after_render_end_ms','wait_after_onscreen_ms')},'total_wait_after_render_end_fraction':sum(p['wait_after_render_end_ms'] for p in pairs)/sum(p['wait_ms'] for p in pairs),'total_wait_after_onscreen_fraction':sum(p['wait_after_onscreen_ms'] for p in pairs)/sum(p['wait_ms'] for p in pairs),'qualification':'Pairs use the same returned native texture pointer across acquisitions, and record actual presentedTime separately from callback arrival. Presentation near acquisition return is correlation; it does not expose exact compositor release time or prove GPU active utilization.'}
 output.write_text(json.dumps(summary,indent=2)+'\n');pairs_output.write_text(json.dumps(pairs,separators=(',',':'))+'\n');print(json.dumps(summary,indent=2));print(pairs[min(10,len(pairs)-1)])

if __name__=='__main__':
 try:main()
 except (OSError,ValueError,KeyError,TypeError,ZeroDivisionError) as error:
  print(f'Drawable correlation failed: {error}',file=sys.stderr)
  sys.exit(2)
