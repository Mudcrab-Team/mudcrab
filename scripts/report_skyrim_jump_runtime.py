#!/usr/bin/env python3
"""Render saved production predictions against the original held-out observations."""

import argparse
import json
from pathlib import Path
import statistics

from check_skyrim_jump_runtime import EVIDENCE, prepare, score


def render():
    _, observations, _ = prepare()
    directory = EVIDENCE / 'implementation'
    predictions = {}
    scores = {}
    for version in ('baseline', 'current'):
        output = json.loads((directory / version / 'predictions.json').read_bytes())
        predictions[version] = {run['id']: run['heights'] for run in output['runs']}
        scores[version] = {run['id']: run for run in score(output, observations)}
    runs = []
    for key, (run, observed) in observations.items():
        if run['split'] != 'heldout':
            continue
        samples = observed['samples']
        times = [sample['controller'] - samples[0]['controller'] for sample in samples]
        keep = [index for index, time in enumerate(times) if 0 <= time <= 1.15]
        runs.append({
            'id': key,
            'dt_ms': statistics.median(sample['controller_dt'] for sample in samples) * 1000,
            'camera_hz': 1 / statistics.median(b['qpc'] - a['qpc'] for a, b in zip(samples, samples[1:])),
            'requested_fps': run['frame_cap'],
            'timescale': run['time_multiplier'],
            'time': [times[index] for index in keep],
            'native': [samples[index]['z'] - observed['baseline'] for index in keep],
            **{version: [predictions[version][key][index] for index in keep]
               for version in predictions},
            'rmse': {version: scores[version][key]['rmse'] for version in scores},
        })
    data = json.dumps(runs, separators=(',', ':'), allow_nan=False).replace('<', '\\u003c')
    passes = {version: sum(row['passes_frozen_report_gate'] for row in values.values()
                          if row['split'] == 'heldout') for version, values in scores.items()}
    counts = f"Before: {passes['baseline']}/{len(runs)} pass. After: {passes['current']}/{len(runs)} pass."
    return '''<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
body{font:14px var(--font-sans,system-ui);color:var(--foreground);margin:0}
label{display:block;margin:8px 0}select{max-width:100%;padding:6px;color:var(--foreground);background:var(--card);border:1px solid var(--border);border-radius:var(--radius,6px)}
svg{display:block;width:100%;height:300px}text{fill:var(--muted-foreground,currentColor);font-size:12px}
.axis{stroke:var(--border,#888);fill:none}.native{stroke:var(--chart-1,#73a9ff)}.baseline{stroke:var(--chart-2,#ef9c58)}.current{stroke:var(--chart-3,#75d2a3)}
.curve{fill:none;stroke-width:2.5}.current{stroke-dasharray:6 4}.legend{display:flex;gap:16px;flex-wrap:wrap}.legend span{border-top:3px solid;padding-top:4px}.legend .native{border-color:var(--chart-1,#73a9ff)}.legend .baseline{border-color:var(--chart-2,#ef9c58)}.legend .current{border-color:var(--chart-3,#75d2a3)}
p{line-height:1.5;margin:10px 0}.small{font-size:12px;color:var(--muted-foreground,currentColor)}
</style></head><body>
<p><strong>Production jump replay · 12 original held-out runs</strong><br>
COUNTS Unchanged gates: RMSE ≤ 1 and maximum error ≤ 3 Creation units.</p>
<label>Recording <select id="run" aria-label="Recording"></select></label>
<div class="legend"><span class="native">Native observation</span><span class="baseline">Old production</span><span class="current">New production</span></div>
<svg viewBox="0 0 680 300" role="img" aria-label="Height above takeoff over game time">
<g id="axes"></g><path class="curve baseline" id="baseline"></path><path class="curve native" id="native"></path><path class="curve current" id="current"></path></svg>
<p id="metrics"></p><p class="small">Rust receives recorded timesteps and one original-training height. It evolves its own position and floor contact. The overlay tests the production transition, not native frame scheduling. Six recordings repeat the training cadence despite 30/120 Hz requests; three use half game speed and three achieve 30 camera Hz. Reusing this corpus is regression validation, not a new blinded experiment.</p>
<script>
const runs=DATA;
const select=document.getElementById('run');
for(const run of runs){const option=document.createElement('option');option.textContent=`${run.id} · requested ${run.requested_fps} Hz, observed ${run.camera_hz.toFixed(1)} camera Hz`;select.append(option);}
const x=t=>52+t/1.15*608,y=h=>258-h/100*230;
const axes=document.getElementById('axes');
for(const h of [0,25,50,75,100])axes.innerHTML+=`<path class="axis" d="M52 ${y(h)}H660"/><text x="42" y="${y(h)+4}" text-anchor="end">${h}</text>`;
for(const t of [0,.25,.5,.75,1])axes.innerHTML+=`<text x="${x(t)}" y="279" text-anchor="middle">${t}</text>`;
axes.innerHTML+='<text x="54" y="16">Height above takeoff · Creation units</text><text x="660" y="298" text-anchor="end">Game seconds</text>';
function draw(){const r=runs[select.selectedIndex];for(const key of ['native','baseline','current'])document.getElementById(key).setAttribute('d',r.time.map((t,i)=>(i?'L':'M')+x(t)+' '+y(r[key][i])).join(' '));document.getElementById('metrics').textContent=`Observed camera ${r.camera_hz.toFixed(1)} Hz (requested ${r.requested_fps}) · median controller step ${r.dt_ms.toFixed(3)} ms · game speed ${r.timescale}× · RMSE before ${r.rmse.baseline.toFixed(3)}, after ${r.rmse.current.toFixed(3)} Creation units`;}
select.addEventListener('change',draw);draw();
</script></body></html>'''.replace('DATA', data).replace('COUNTS', counts)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(render())


if __name__ == '__main__':
    main()
