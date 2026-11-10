#!/usr/bin/env python3
"""Render the sealed jump scores and numerical traces as a portable evidence chart."""
import argparse
import json
from pathlib import Path
import statistics

import evaluate_skyrim_jump as evaluator


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--results', type=Path, required=True)
    args = parser.parse_args()
    manifest, root = evaluator.manifest(args.corpus / 'manifest.json')
    frozen = json.loads((args.corpus / 'frozen.json').read_text())
    result = json.loads((args.results / 'report.json').read_text())
    scores = {s['id']: s for s in frozen['training_scores'] + result['runs']}
    groups = {}
    curves = []
    for run in manifest['runs']:
        score = scores[run['id']]
        if 'rejected' in score:
            continue
        condition = f"requested {run['frame_cap']} Hz, {run['time_multiplier']:g}x, {run['split']}"
        groups.setdefault(condition, []).append(score)
        jump = evaluator.qualify(root / run['path'], run)
        samples, origin = jump['samples'], jump['origin']
        native = next(m for m in frozen['models'] if m['law'] == 'native_discrete')
        mudcrab = next(m for m in frozen['models'] if m['law'] == 'mudcrab' and m['clock'] == 'camera')
        predicted = evaluator.native_forecast(native, [s['controller_dt'] if s['sample'] > origin['sample'] else None for s in samples])
        baseline = evaluator.forecast(mudcrab, [s['camera'] - origin['camera'] for s in samples])
        points = [[s['controller'] - origin['controller'], s['z'] - jump['baseline'], p, b]
                  for s,p,b in zip(samples,predicted,baseline)
                  if -.05 <= s['controller'] - origin['controller'] <= 1.15]
        curves.append({'id': run['id'], 'condition': condition, 'points': points})
    summary = []
    for condition, runs in groups.items():
        def selected(law, clock):
            return [next(m for m in r['models'] if m['model']['law'] == law and m['model']['clock'] == clock)
                    for r in runs]
        native, mudcrab = selected('native_discrete', 'controller'), selected('mudcrab', 'camera')
        apex = [r['observed_sampled_apex'] for r in runs]
        summary.append({'condition': condition, 'n': len(runs),
                        'apex_min': min(apex), 'apex_median': statistics.median(apex), 'apex_max': max(apex),
                        'native_rmse_median': statistics.median(m['window']['rmse'] for m in native),
                        'native_max_error': max(m['window']['max_abs'] for m in native),
                        'native_flight_rmse_max': max(m['whole_flight']['rmse'] for m in native),
                        'mudcrab_rmse_median': statistics.median(m['window']['rmse'] for m in mudcrab),
                        'mudcrab_max_error': max(m['window']['max_abs'] for m in mudcrab),
                        'native_passes': sum(m['passes_frozen_report_gate'] for m in native),
                        'observed_camera_hz_median': statistics.median(1 / r['clocks']['qpc_dt']['median'] for r in runs),
                        'camera_to_wall_ratio_median': statistics.median(r['clocks']['camera_to_qpc_ratio']['median'] for r in runs),
                        'observed_wall_dt_median': statistics.median(r['clocks']['qpc_dt']['median'] for r in runs),
                        'observed_controller_dt_median': statistics.median(r['clocks']['controller_dt']['median'] for r in runs)})
    comparisons = []
    for condition, runs in groups.items():
        for model in frozen['models']:
            values = [next(m for m in r['models'] if m['model'] == model) for r in runs]
            comparisons.append({'condition': condition, 'law': model['law'], 'clock': model['clock'],
                                'passes': sum(m['passes_frozen_report_gate'] for m in values), 'n': len(values),
                                'rmse_median': statistics.median(m['window']['rmse'] for m in values),
                                'max_abs_error': max(m['window']['max_abs'] for m in values)})
    (args.results / 'comparisons.json').write_text(json.dumps(comparisons, indent=2) + '\n')
    (args.results / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    (args.results / 'curves.json').write_text(json.dumps(curves, separators=(',', ':')) + '\n')
    html = '''<!doctype html><html><head><meta charset="utf-8"><style>
:root{color-scheme:dark;--foreground:#e2e8f0;--muted-foreground:#94a3b8;--border:#334155;--chart-1:#38bdf8;--chart-2:#4ade80;--chart-3:#fbbf24;--font-sans:system-ui;--font-mono:monospace}
body{color:var(--foreground);font-family:var(--font-sans);margin:0}select{font:inherit;max-width:100%;color:var(--foreground);background:var(--background,#0f172a);border:1px solid var(--border);padding:6px}svg{display:block;width:100%;height:285px}.muted{color:var(--muted-foreground);font-size:13px}table{border-collapse:collapse;font-size:13px;width:100%;margin:10px 0}th,td{padding:6px 4px;text-align:right;border-bottom:1px solid var(--border)}th:first-child,td:first-child{text-align:left}.scroll{overflow-x:auto}text{fill:var(--muted-foreground);font-family:var(--font-sans);font-size:11px}
</style></head><body><label>Native jump run <select id="run"></select></label>
<svg id="curve" viewBox="0 0 700 285" role="img" aria-label="Player jump height over controller time"></svg>
<p class="muted"><span style="color:var(--chart-1)">● Native player height</span> &nbsp; <span style="color:var(--chart-2)">Frozen discrete evaluator</span> &nbsp; <span style="color:var(--chart-3)">Mudcrab 900 / 340 at 60 Hz</span></p>
<svg id="apex" viewBox="0 0 700 285" role="img" aria-label="Observed jump apex across cadence conditions"></svg>
<div class="scroll"><table id="table"></table></div>
<p class="muted">Each condition has three independent captures. Bars show observed sampled apex; whiskers show the repeat range. Hz labels on bars are measured camera cadence, not requested caps. Prediction uses frozen parameters, launch boundary and recorded controller timesteps. No future observed height or velocity enters it. Native contact placement differs from the evaluator's idealized floor.</p>
<script>const curves=CURVES;const summary=SUMMARY;
const ns='http://www.w3.org/2000/svg';function add(svg,type,attrs,text){const e=document.createElementNS(ns,type);for(const [k,v]of Object.entries(attrs))e.setAttribute(k,v);if(text!==undefined)e.textContent=text;svg.appendChild(e);return e}
const chooser=document.getElementById('run');curves.forEach((c,i)=>{const o=document.createElement('option');o.value=i;o.textContent=c.id;chooser.appendChild(o)});
function chart(){const s=document.getElementById('curve');s.replaceChildren();const data=curves[+chooser.value].points;const x=t=>55+t/1.15*620,y=h=>240-h/90*210;
for(let h=0;h<=80;h+=20){add(s,'line',{x1:55,x2:675,y1:y(h),y2:y(h),stroke:'var(--border)'});add(s,'text',{x:46,y:y(h)+4,'text-anchor':'end'},h)}
for(let t=0;t<=1.01;t+=.2)add(s,'text',{x:x(t),y:258,'text-anchor':'middle'},t.toFixed(1));
add(s,'text',{x:55,y:15},'Height above launch floor · Creation units');add(s,'text',{x:365,y:280,'text-anchor':'middle'},'Elapsed controller time · seconds');
for(const col of [3,2])add(s,'polyline',{points:data.filter(p=>p[0]>=0).map(p=>x(p[0])+','+y(p[col])).join(' '),fill:'none',stroke:`var(--chart-${col===2?2:3})`,'stroke-width':2});
data.filter(p=>p[0]>=0).forEach(p=>add(s,'circle',{cx:x(p[0]),cy:y(p[1]),r:2,fill:'var(--chart-1)'}));}
chooser.addEventListener('change',chart);chart();
const a=document.getElementById('apex'),bottom=220,scale=2;
for(let h=0;h<=80;h+=20){add(a,'line',{x1:55,x2:675,y1:bottom-h*scale,y2:bottom-h*scale,stroke:'var(--border)'});add(a,'text',{x:46,y:bottom-h*scale+4,'text-anchor':'end'},h)}
add(a,'text',{x:55,y:25},'Observed sampled apex · Creation units');summary.forEach((r,i)=>{const x=55+(i+.5)*620/summary.length;add(a,'rect',{x:x-35,y:bottom-r.apex_median*scale,width:70,height:r.apex_median*scale,fill:'var(--chart-1)',opacity:.75});add(a,'line',{x1:x,x2:x,y1:bottom-r.apex_min*scale,y2:bottom-r.apex_max*scale,stroke:'var(--foreground)','stroke-width':3});add(a,'text',{x,y:bottom-r.apex_median*scale-9,'text-anchor':'middle'},r.apex_median.toFixed(3));add(a,'text',{x,y:242,'text-anchor':'middle'},r.observed_camera_hz_median.toFixed(1)+' Hz / '+r.condition.split(', ')[1]);add(a,'text',{x,y:258,'text-anchor':'middle'},r.condition.includes('heldout')?'held out':'training')});
document.getElementById('table').innerHTML='<thead><tr><th>Requested condition</th><th>Observed Hz</th><th>Apex</th><th>Evaluator RMSE</th><th>Mudcrab RMSE</th></tr></thead><tbody>'+summary.map(r=>`<tr><td>${r.condition}</td><td>${r.observed_camera_hz_median.toFixed(2)}</td><td>${r.apex_median.toFixed(3)}</td><td>${r.native_rmse_median.toFixed(4)}</td><td>${r.mudcrab_rmse_median.toFixed(3)}</td></tr>`).join('')+'</tbody>';
</script></body></html>'''.replace('CURVES', json.dumps(curves)).replace('SUMMARY', json.dumps(summary))
    (args.results / 'report.html').write_text(html)
    print(args.results / 'report.html')


if __name__ == '__main__':
    main()
