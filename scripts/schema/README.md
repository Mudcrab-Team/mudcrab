# Schema pilot checks

Run the offline contract and corpus-manifest tests with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/schema -p 'test_*.py' -v
```

The offline tests use only repository fixtures and Python's standard library;
CI runs them on Linux/Python 3.11 and Windows/Python 3.13.

The separate synthetic Mutagen qualification is an author-run evidence tool.
It requires .NET SDK 9.0.318 and a separately supplied, hash-pinned
`oracles/records` source that is not included in this public repository. Its
recorded execution cannot yet be reproduced from this repository alone. The
CLI requires an explicit `--oracle-source`; there is no machine-local default.
It restores Mutagen 0.54.4 into a fresh
temporary artifact directory, builds a guarded source copy, and inspects only
the hand-encoded fixtures in this pilot:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/schema/run_mutagen_p0.py \
  --dotnet /path/to/dotnet \
  --oracle-source /path/to/supplied/oracles/records
```

The command prints its artifact directory on success. An explicit
`--artifact-dir` must name a new or empty path outside the repository, the RE
project, the oracle source, and the local game store. The runner keeps failed
or timed-out observations marked incomplete and stores raw output separately
from accepted JSON observations.

This pilot reports xEdit as unavailable because no executable or qualified
runner is present. Mutagen typed traversal does not establish physical source
framing, complete record-occurrence coverage, full-catalog acceptance, retail
corpus acceptance, or native-runtime compatibility. All fixture inputs are
synthetic.

The localized ARMO carries hand-encoded string ID `0x12345678` and no string
table. Its report keeps that fixture value separate from Mutagen's optional
`StringsKey`, the printable `Name`, and translated text. An empty printable
name is recorded as unavailable; it does not mean the field is absent.
