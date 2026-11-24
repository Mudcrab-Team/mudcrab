# Compare Papyrus behavior with Skyrim

These original scripts test a small set of runtime contracts. They do not contain game scripts or assets. The same compiled PEX files run in Skyrim and through Mudcrab's current PEX converter and shared Luau helper.

| Fixture | What the trace distinguishes |
| --- | --- |
| `MCParityInheritance` with `MCParityParent` and `MCParityGrand` | Active-state ancestry versus empty-state fallback, inherited calls, virtual receiver identity, lexical `Parent` calls, and empty-state dispatch. |
| `MCParityNestedEnd` | A guarded nested `GotoState` from `OnEndState`, including the state seen before and after re-entry. |
| `MCParityNestedBegin` | A nested `GotoState` from `OnBeginState`, including the state retained when the outer callback returns. |
| `MCParityLatent` | Caller/callee order and return-value delivery around `Outer` calling `Inner`, which calls `Utility.Wait`. |

Each quest guards `OnInit` with `hasRun`. A fresh Skyrim quest can receive initialization both when bound and when started. The comparator rejects duplicated or incomplete executions instead of selecting a convenient subsequence.

## Compile the fixtures

Use [Caprica v0.3.0](https://github.com/Orvid/Caprica/releases/tag/v0.3.0) in Skyrim mode. Set absolute paths for the compiler, this tool directory, and a private output directory. The compiler-only headers declare the small native interface used by the fixtures. Do not compile or deploy them as stock game scripts.

```sh
"$CAPRICA" --ignorecwd --game skyrim \
	--flags "$PROBES/headers/Parity.flg" \
	--import "$PROBES/headers" --import "$PROBES/fixtures" \
	--output "$PEX" --dump-asm "$PROBES/fixtures"
```

The release is a Windows executable. On Linux, run it through an initialized Proton prefix. Use Windows paths for its arguments. In the measured run, stdout was not forwarded through Proton, so a private `cmd /c` batch redirected compiler output and saved `%ERRORLEVEL%`. Verify exit code zero and all six `MCParity*.pex` outputs. Do not add `--release`, which can remove diagnostic calls. Preserve compiler version, executable hash, source hashes, and assembly dumps with the result.

## Prepare an isolated game profile

Use a clean, initialized Proton compatdata directory as the prefix template. The command below creates a new game directory and copied prefix. It links game archives, copies only the six fixture PEX files, generates `MCParity.esp`, and enables Papyrus logging in the copied profile. Existing output directories are refused.

```sh
python3 tools/papyrus-parity/prepare_skyrim.py \
	--game "$SKYRIM" --pex "$PEX" \
	--prefix-template "$CLEAN_COMPATDATA" --output "$SESSION"
```

`inputs.json` pins the executable, plugin, and deployed PEX hashes. The profile loads stock masters and the probe plugin. The launch uses the stock executable, not SKSE. The copied prefix's Documents and AppData paths must stay inside that prefix. Logs stay under its `Documents/My Games/Skyrim Special Edition/Logs/Script` directory.

Start the normal Steam client and allow its existing session to sign in. Launch the disposable executable with the copied prefix and its own working directory:

```sh
cd "$SESSION/game"
DISPLAY="$PROBE_DISPLAY" PROTON_USE_XALIA=0 \
	STEAM_COMPAT_DATA_PATH="$SESSION/compatdata" \
	STEAM_COMPAT_CLIENT_INSTALL_PATH="$STEAM" \
	STEAM_COMPAT_INSTALL_PATH="$SESSION/game" \
	SteamAppId=489830 SteamGameId=489830 \
	steam-run "$PROTON/proton" run "$SESSION/game/SkyrimSE.exe"
```

The measured Linux run used a private Xvfb display with Openbox. Without a window manager the game rendered but did not accept the injected keys. Hold console and Return keys long enough for the game to observe them. Verify an open, empty console before typing `coc QASmoke`. This starts a fresh game without loading or altering a normal save.

Wait until each of the four case names has exactly one `begin` and one `done` marker. Copy `Papyrus.0.log` to a new evidence path. Preserve the launch receipt and input hashes. Quit with `qqq` without saving. Before relaunching, let that prefix's Wine server exit. If needed, stop and wait for only that disposable prefix's Wine server.

## Run the current converter and compare

Run from the repository root. Both commands require a new output filename for each run.

```sh
cargo run --locked -p converter --example papyrus_parity -- "$PEX" "$AUDIT/luau.json"
python3 -B tools/papyrus-parity/compare_traces.py \
	--retail "$AUDIT/Papyrus.0.log" --luau "$AUDIT/luau.json" \
	--deployed "$SESSION/inputs.json" --output "$AUDIT/comparison.json"
```

The runner copies the six PEX inputs into a private temporary snapshot, then uses that snapshot for every conversion and its input hashes. It executes the shared helper and records script traces, failures, yields, and hashes. Debug output is a capture stub. The primary run does not supply a `Utility::Wait` provider. A separate probe-only Luau adapter yields and resumes the coroutine to test continuation ordering. The comparator requires exactly one wait of 0.5 with the pre-resume trace ending at `inner.before`. Its success never counts as shipped engine support.

The comparator rechecks the deployed executable, plugin, and PEX hashes. It requires identical Skyrim and Luau bytecode inputs and compares order separately for each quest. Exit codes are 0 for a complete match, 1 for measured differences or unsupported native calls, and 2 for incomplete evidence. It does not compare elapsed time or global ordering across concurrently initialized quests.

Run the comparator controls and helper regressions:

```sh
python3 -B -m unittest discover -s tools/papyrus-parity -p 'test_*.py' -v
cargo test --locked -p converter --test papyrus_states
```

Keep raw logs, compiled fixture outputs, prefixes, screenshots, and saves outside Git. The [2026-11-17 report](../../docs/research/papyrus-parity-20261117.md) records the measured outcome and remaining gaps.
