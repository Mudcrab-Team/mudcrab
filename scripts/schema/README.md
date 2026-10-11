# Schema pilot checks

Run the offline contract and corpus-manifest tests with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/schema -p 'test_*.py' -v
```

The offline tests use only repository fixtures and Python's standard library;
CI runs them on Linux/Python 3.11 and Windows/Python 3.13.

The corpus manifest records stat-checked file reads, not an immutable source
snapshot. Discovery skips symlink directories it observes, and the file reader
rejects a symlink at the final path component. It does not pin ancestor
directories: replacing a parent directory with a symlink after discovery can
redirect a read outside Data while the manifest retains the original
Data-relative path. Each hash identifies the bytes read from the opened file;
the recorded path does not prove unchanged ancestry. This residual limit is
reported under `unresolved`; it does not mean a swap was detected in that run.
Use a stable source tree for P0 observations. Immutable source retention remains
P1 work.

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
`--artifact-dir` must name a new or empty path outside every registered Mudcrab
worktree, the RE project, the oracle/tool source, supplied SDK/Wine executable
directories, and the local game store. A
failed worktree lookup rejects the destination. The runner keeps failed
or timed-out observations marked incomplete and stores raw output separately
from accepted JSON observations.

The separate xEdit capability probe uses the official `xedit-4.1.5f` release's
`xDump64.exe` (20,510,720 bytes; SHA-256
`30c085b8a20dc02bf5abae2cb6610870c9bb9eea50330e0fe5ade98e3f89efe6`)
with Wine. It neither downloads a tool nor changes the installed game:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/schema/run_xedit_p0.py \
  --xdump /path/to/xDump64.exe \
  --wine /path/to/wine \
  --artifact-dir /tmp/mudcrab-xedit-probe
```

The binary is pinned separately from the newer mined source declarations. The
release tag is metadata; its source-to-binary build relationship is unverified.
The probe copies the verified binary, creates an isolated Wine prefix, and
redirects XDG cache/config and all three process temp variables into its guarded
artifact directory. It records stdout/stderr, input digests, known-value checks,
schema diagnostics, and hashes of the Python decision code in `probe-results.json`.
Its current verdict is deliberately unqualified (exit 2): dumped values do not
certify the whole fixture, and a zero exit after a crash or malformed dump does
not establish managed rejection. The [delivery notes](../../docs/research/dynamic-schema/p0-next-delivery.md)
record the executed cases and qualification gaps.

Both tool runners bound commands and output collection after timeout. They
signal the supervised process group. An escaped descendant can survive that
signal; inherited pipes cannot keep the verdict path waiting indefinitely.
Stopping arbitrary escaped descendants is not part of this supervisor's contract.

Mutagen typed traversal does not establish physical source
framing, complete record-occurrence coverage, full-catalog acceptance, retail
corpus acceptance, or native-runtime compatibility. All fixture inputs are
synthetic.

The localized ARMO carries hand-encoded string ID `0x12345678` and no string
table. Its report keeps that fixture value separate from Mutagen's optional
`StringsKey`, the printable `Name`, and translated text. An empty printable
name is recorded as unavailable; it does not mean the field is absent.

The corpus tool's `--corpus-evidence` input accepts version-1 strict JSON with
these keys:

| Key | Required contents |
| --- | --- |
| `schema_version` | Integer `1` |
| `corpus_profile` | `id`, fixed `target`, and `evidence` |
| `locale` | Declared `value` and `evidence` |
| `load_order` | Ordered `active_plugins`, `unloaded_optional_plugins`, and `evidence` |
| Each `evidence` | `path` and exact lowercase SHA-256; relative paths resolve beside the descriptor |
| Profile `target` | `game: "Skyrim Special Edition"`, `executable_version: "1.7.104.0"`, `steam_build: "24914197"` |

Every installed plugin must be classified; active masters precede their
dependents. Duplicate keys/names, unknown keys, incompatible targets, missing or
changed evidence, oversized/deep JSON, and output overlap with evidence fail.
Matching bytes establish the supplied artifact identity. Locale, active-order,
corpus-profile and official-content claims remain semantically unverified;
this input cannot make a candidate manifest accepted.
