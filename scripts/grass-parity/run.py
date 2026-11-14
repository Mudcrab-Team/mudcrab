"""Rerun both CPU grass dumps and the comparison. No renderer or game is launched."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from datetime import datetime, timezone


def sha(path):
    """Hash a retained input or executable with bounded memory."""
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    """Require explicit cache and output locations and preserve executable provenance."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('ours-bin', 'grass-cache', 'skytool', 'cache', 'world', 'cells', 'out'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--ours-cells', type=Path, help='optional cell list with explicit source-world IDs; same XY scope')
    args = parser.parse_args()
    ours_cells = args.ours_cells or args.cells
    def coordinates(path):
        """Input alignment may change world IDs, never the requested coordinate scope."""
        return {tuple(map(int, line.split(',')[:2])) for line in path.read_text(encoding='utf-8-sig').splitlines() if line.strip()}
    if coordinates(ours_cells) != coordinates(args.cells):
        parser.error('--ours-cells must contain the same cell coordinates as --cells')
    if args.out.exists():
        parser.error('--out must be a new directory to preserve previous comparisons')
    local = Path(os.environ['LOCALAPPDATA']) / 'spud'
    if local.exists():
        parser.error('default spud cache already exists; no executable was run')
    args.out.mkdir(parents=True)
    ours, spud = args.out / 'ours', args.out / 'spud'
    ours.mkdir()
    spud.mkdir()
    shutil.copyfile(ours_cells, ours / 'requested-cells.csv')
    shutil.copyfile(args.cells, spud / 'requested-cells.csv')
    commands = [
        [str(args.skytool.resolve()), '--cache', str(args.cache.resolve()), 'grass-dump',
         str(args.world.resolve()), str(args.cells.resolve()), str(spud.resolve())],
        [str(args.ours_bin.resolve()), str(args.grass_cache.resolve()),
         str(ours_cells.resolve()), str(ours.resolve())],
        [sys.executable, '-B', str(Path(__file__).with_name('compare.py')),
         '--ours', str(ours), '--spud', str(spud), '--out', str(args.out / 'comparison')],
    ]
    receipt = dict(utc=datetime.now(timezone.utc).isoformat(), commands=commands,
                   c_spud_before_exists=False, inputs={str(p.resolve()): sha(p) for p in (
                       args.ours_bin, args.skytool, args.grass_cache, args.world, args.cells, ours_cells)},
                   exit_codes=[])
    try:
        for index, command in enumerate(commands):
            with (args.out / f'command-{index}.log').open('w', encoding='utf-8') as log:
                result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=False)
            receipt['exit_codes'].append(result.returncode)
            if result.returncode:
                raise RuntimeError(f'command {index} failed; see command-{index}.log')
            if local.exists():
                raise RuntimeError('default spud cache appeared')
        receipt['outputs'] = {str(p.relative_to(args.out)): sha(p)
                              for p in sorted(args.out.rglob('*.csv'))}
    finally:
        receipt['c_spud_after_exists'] = local.exists()
        (args.out / 'receipt.json').write_text(json.dumps(receipt, indent=2), encoding='utf-8')
    print(args.out / 'comparison' / 'COMPARE.md')


if __name__ == '__main__':
    main()
