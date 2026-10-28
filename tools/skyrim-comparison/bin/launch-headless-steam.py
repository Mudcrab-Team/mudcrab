import os
from pathlib import Path
r=Path("/home/taylor/Games/MudcrabComparison")
e=os.environ.copy()
values=dict(s.split("=",1) for s in (r/"artifacts/display.env").read_text().splitlines() if "=" in s)
e.update({k:values[k] for k in ["DISPLAY","WAYLAND_DISPLAY"] if k in values})
os.execve("/run/current-system/sw/bin/steam",["steam","-silent"],e)
