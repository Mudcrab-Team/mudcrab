import os,time,subprocess
from pathlib import Path
r=Path('/home/taylor/Games/MudcrabComparison')
e=os.environ.copy()
e['DXVK_FRAME_RATE']='60'
values=dict(s.split('=',1) for s in (r/'artifacts/display.env').read_text().splitlines() if '=' in s)
e.update({k:values[k] for k in ['DISPLAY','WAYLAND_DISPLAY'] if k in values})
for _ in range(60):
 if subprocess.run(['pgrep','-x','steam'],capture_output=True).returncode==0:break
 time.sleep(1)
else:raise RuntimeError('Steam startup timed out')
os.execve(str(r/'bin/launch-comparison'),[str(r/'bin/launch-comparison')],e)
