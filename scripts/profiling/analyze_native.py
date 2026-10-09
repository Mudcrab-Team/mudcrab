#!/usr/bin/env python3
import argparse,collections,json,statistics,math,sys
from pathlib import Path

def main():
 p=argparse.ArgumentParser();p.add_argument('input');p.add_argument('--output',required=True);a=p.parse_args()
 cb={};acq={};present={};health=[];types=collections.Counter();enc_labels=collections.Counter();begin_clock=None
 for line in Path(a.input).open():
  r=json.loads(line);k=r['type'];types[k]+=1
  if k=='trace_begin':begin_clock=r
  elif k=='health':health.append(r)
  elif k=='acquire_begin':acq.setdefault(r['acquisition_id'],{})['begin']=r
  elif k=='acquire_end':acq.setdefault(r['acquisition_id'],{})['end']=r
  elif k=='presented':present[r['acquisition_id']]=r
  elif k in ('encoder_end','completed','commit_begin','commit_end','present_encode_begin'):
   c=cb.setdefault(r['cb_id'],{'enc':[]})
   if k=='encoder_end':
    c['enc'].append({'name':r['name'],'selector':r['selector'],'begin_ns':r['host_ns'],'end_ns':r['end_ns'],'object':r['object'],'cpu_ns':r['thread_cpu_ns']-r['extra'] if r['thread']==r.get('count') else None,'acquisition_id':r['acquisition_id']});enc_labels[r['name']]+=1
   else:c[k]=r

 def stats(v):
  v=sorted(v)
  if not v:return None
  return {'n':len(v),'mean':statistics.mean(v),'p50':statistics.median(v),'p95':v[max(0,math.ceil(.95*len(v))-1)],'min':v[0],'max':v[-1]}
 if begin_clock is None:raise ValueError('Missing trace_begin record; capture provenance and clock are unknown')
 if not health:raise ValueError('Missing health records; capture completeness cannot be checked')
 for record in health:
  for counter in ('dropped','exceptions','hook_failures','thread_cpu_clock_failures','capture_config_errors'):
   if record.get(counter,0):raise ValueError(f'Unhealthy capture: {counter}={record[counter]}')
 valid=[c for c in cb.values() if c.get('completed',{}).get('gpu_time_valid')]
 acquire=[r for r in acq.values() if 'begin' in r and 'end' in r]
 if not acquire:raise ValueError('No complete acquire_begin/acquire_end pairs; trace may have no drawables or start after exit')
 incomplete=len(acq)-len(acquire)
 if incomplete:raise ValueError(f'{incomplete} incomplete acquisition pairs; use a complete zero-drop capture')
 # Drop first/last one-second observation tails to avoid ownership of partial frames.
 lo=min(r['end']['end_ns'] for r in acquire)+1000000000
 hi=max(r['end']['end_ns'] for r in acquire)-1000000000
 if hi<=lo:raise ValueError('Need more than two seconds of paired acquisitions for one-second tail cropping')
 accepted=[c for c in valid if lo<=c['completed']['gpu_start_s']*1e9 and c['completed']['gpu_end_s']*1e9<=hi]
 agg=collections.defaultdict(lambda:{'gpu_ms':[],'commit_ms':[],'queue_ms':[],'encoder_cpu_ms':[],'encoder_wall_ms':[]})
 frame_counts=collections.Counter();slim=[]
 for c in accepted:
  end=c['completed'];enc=c['enc'];unique={(e['object'],e['begin_ns'],e['end_ns']):e for e in enc};enc=list(unique.values())
  name='empty' if not enc else enc[0]['name'] or enc[0]['selector'] if len(enc)==1 else 'multiple: '+', '.join(e['name'] or e['selector'] for e in enc)
  gpu_ms=(end['gpu_end_s']-end['gpu_start_s'])*1000
  agg[name]['gpu_ms'].append(gpu_ms)
  start=c.get('commit_begin',{});finish=c.get('commit_end',{})
  if start and finish:agg[name]['commit_ms'].append((finish['end_ns']-start['host_ns'])/1e6)
  if start:agg[name]['queue_ms'].append((end['gpu_start_s']*1e9-start['host_ns'])/1e6)
  for e in enc:
   if e['cpu_ns'] is not None and e['cpu_ns']>=0:agg[name]['encoder_cpu_ms'].append(e['cpu_ns']/1e6)
   agg[name]['encoder_wall_ms'].append((e['end_ns']-e['begin_ns'])/1e6)
  slim.append({'cb_id':end['cb_id'],'label':name,'gpu_start_s':end['gpu_start_s'],'gpu_end_s':end['gpu_end_s'],'commit_ns':start.get('host_ns'),'acquisition_id':end['acquisition_id'] or next((e['acquisition_id'] for e in enc if e['acquisition_id']),0),'cb_name':end['name'],'commit_end_ns':finish.get('end_ns')})
 # GPU interval union; never sum overlapping command-buffer intervals as frame time.
 intervals=sorted((c['completed']['gpu_start_s'],c['completed']['gpu_end_s']) for c in accepted)
 merged=[]
 for x,y in intervals:
  if merged and x<=merged[-1][1]:merged[-1][1]=max(y,merged[-1][1])
  else:merged.append([x,y])
 summary={'schema':1,'source':a.input,'clock':begin_clock,'event_counts':dict(types),'health_last':health[-1],'window_host_ns':[lo,hi],'window_s':(hi-lo)/1e9,'acquisition_count':len(acquire),'command_buffers_with_valid_gpu_time':len(valid),'accepted_gpu_buffers':len(accepted),'accepted_gpu_interval_union_s':sum(y-x for x,y in merged),'accepted_gpu_interval_sum_s':sum(y-x for x,y in intervals),'acquire_wait_ms':stats([(r['end']['end_ns']-r['begin']['host_ns'])/1e6 for r in acquire if lo<=r['begin']['host_ns'] and r['end']['end_ns']<=hi]),'acquire_cpu_ms':stats([(r['end']['thread_cpu_ns']-r['begin']['thread_cpu_ns'])/1e6 for r in acquire if lo<=r['begin']['host_ns'] and r['end']['end_ns']<=hi]),'encoder_labels':enc_labels.most_common(),'groups':{k:{n:stats(v) for n,v in values.items()} for k,values in agg.items()},'qualification':'Observed command-buffer elapsed GPU spans include idle, dependency waits, and overlap. Dropped event rows can make pass labels incomplete. Empty means no covered encoder_end event; not proof of no GPU work. No per-shader active-cycle measurements.'}
 out=Path(a.output);out.write_text(json.dumps(summary,indent=2)+'\n');out.with_name(out.stem+'-timeline.json').write_text(json.dumps({'window_host_ns':[lo,hi],'command_buffers':slim,'acquisitions':[r for r in acquire if lo<=r['begin']['host_ns'] and r['end']['end_ns']<=hi],'presented':[r for r in present.values() if lo<=r['host_ns']<=hi]},separators=(',',':'))+'\n')
 print(json.dumps({k:summary[k] for k in ('window_s','accepted_gpu_buffers','accepted_gpu_interval_union_s','accepted_gpu_interval_sum_s','acquire_wait_ms','acquire_cpu_ms','health_last')},indent=2))
 print('Top aggregate elapsed spans (nonadditive):')
 for name,r in sorted(summary['groups'].items(),key=lambda item:item[1]['gpu_ms']['mean']*item[1]['gpu_ms']['n'],reverse=True)[:15]:print(name, r['gpu_ms'])

if __name__=='__main__':
 try:main()
 except (OSError,ValueError,KeyError,TypeError,ZeroDivisionError) as error:
  print(f'Native trace analysis failed: {error}',file=sys.stderr)
  sys.exit(2)
