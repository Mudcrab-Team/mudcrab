# REA assessment for Skyrim compatibility

Assessed 2026-10-08 against Mudcrab `f40ca1df770e` and REA
[`be783ef`](https://github.com/morluto/rea/tree/be783efaba62dc76c5d8d42229869a32afdb302a).
The npm registry reports `rea-agents` **6.0.0**, checkpoint
`6fee42689ae6e95a0182e0d4d0f55644e73e72be`. REA's installation document still
describes the previous 5.0.0 release; select capabilities from the running server.

This assessment preceded installation and analysis. The subsequent
[movement investigation](skyrim-movement-native-20261008.md) records the executed
pilot, including the PE smoke result, REA's Skyrim startup timeout and the native
behavior findings.

## Recommendation

Use REA as a research tool for narrow questions about native Skyrim behavior.
Start with movement-type selection and one speed consumer. Those questions
correspond to explicit assumptions in the current engine and have measurable
answers. Adopt it as a development tool outside the engine/converter dependencies.

REA connects an agent to Ghidra, Hopper or IDA for function analysis,
decompilation, assembly, strings and references. It does not supply a Skyrim
model. Its value here is making targeted native investigations easier to repeat
and cite. Existing record schemas and reference implementations remain the first
source for questions they already answer.

Sources: [REA README][rea-readme], [published package metadata][npm],
[6.0.0 release][release].

## What can run where

| Workflow | Fit for this project | Limits |
| --- | --- | --- |
| Ghidra on macOS arm64 or Linux x64, analyzing a native x64 PE | Static investigation of `SkyrimSE.exe`; the analyst host need not run the game | Requires an existing Ghidra 12.1.x installation, full 64-bit JDK, and the host's native decompiler |
| Ghidra on Windows x64 | Experimental static analysis of native PE applications | Fixed local NTFS paths; P0 excludes DLL targets and annotations |
| REA native call observation | Available for macOS Mach-O targets launched under LLDB | Does not provide Windows Skyrim call tracing |
| Shader and rendered-frame investigation | Combine source references with separate retail shader evidence and GPU capture | REA has no documented Direct3D frame-capture or shader-parity workflow |

Supported Node runtimes are 22.x from 22.19, 24.x from 24.11, or 26+.
Ghidra and Java are separate prerequisites. macOS provider supervision also
uses `xcrun`/Swift from Apple's command-line tools. The reviewed implementation
accepts x64 PE targets on the supported macOS/Linux hosts.

REA creates an ephemeral Ghidra project and completes default auto-analysis
before returning its first native result. Startup has a **330,000 ms** deadline.
Whether a full Skyrim executable fits that deadline is untested. A successful
`open_binary` establishes target selection, not completed analysis.

Keep one MCP session open during an investigation. Fresh sessions import again;
CLI invocations are separate sessions. Snapshots reuse matching query results,
not the Ghidra project. Function annotations on macOS/Linux disappear on close.
Do not assume that REA imports CommonLib types or Address Library labels: use
version-matched addresses as seeds, and verify each against the selected binary.

Sources: [installation][installation], [Windows P0][windows],
[native investigation][native], [CLI snapshots][cli],
[first-query deadlines][contracts], [Ghidra provider source][provider].

## Questions worth investigating

### 1. Movement selection and speed use

[The movement record map](../player-movement-record-map.md) states that choosing
`NPC_Default_MT` for an unset race movement link is a project rule. It also leaves
the physical interpretation of `SPED`, gravity and jump launch provisional.
[The controller](../../crates/engine/src/physics.rs) rejects explicit `WKMV` or
`RNMV` links and supplies its own directional blend and sprint multiplier.

First establish how an unset `TESRace::baseMoveTypes` slot resolves a movement
form, then trace one consumer of the forward walk/run fields. Record field
offsets, branch conditions and scaling operations. Later compare steady-state
distance at several frame rates and jump height/time against the retail game.
Static evidence alone cannot establish which path runs for a particular actor.

Use the record map's pinned CommonLib `Movement.h` and `TESRace.h`, and Mutagen's
MOVT schema, before searching the executable. Record layout evidence does not
establish the fallback-selection path or elapsed-time equation.

### 2. Light attenuation and shader constants

[The light specification](../specs/engine/point-lights.md) leaves authored
attenuation exponent, fade, flicker and cone behavior unused.
[The implementation](../../crates/engine/src/lights.rs) maps radius to an
approximate intensity and uses Bevy's light response.

Trace one light record's fields into the native render constants. Then use a
controlled vanilla scene to sample brightness against distance. REA can help
identify CPU dataflow; the final pixel equation needs the corresponding shader
and frame evidence.

### 3. Model-space normals and specular response

[L1 compatibility](../lighting-l1-compatibility.md) names model-space normals and
native BRDF/intensity as remaining gaps. The current
[material hook](../../crates/engine/src/nif_material.rs) constructs Bevy
`StandardMaterial` values.

Start with the project's pinned [Community Shaders source][community-shaders].
That reference helps identify fields and equations, but its modified rendering
does not prove vanilla parity. Compare one retail model-space permutation and
one ordinary specular permutation, preserving their constant-buffer layouts and
normal/mask conventions. Use REA for native permutation selection and constant
setup; use separate shader tools for the GPU instructions.

### 4. Stair contact filtering

[The Riverwood collision note](../riverwood-static-collision.md) records both
`WOOD` and `STAIRS_WOOD` as physical because native filtering is unproven.
The published [collision mesh contract](../../crates/shared/src/collision.rs)
does not retain per-triangle materials. A confirmed material-specific native
rule could therefore require a converter/contract change as well as a controller
change.

Investigate one material/layer decision only if controlled stair traversal
exposes a mismatch. Validate repeated ascent/descent, contact material and
vertical displacement. A material's name does not establish its controller role.

### 5. Papyrus semantics

The [transpiler](../../crates/converter/src/script.rs) emits named-state methods
as `State::Function`, while [runtime dispatch](../../crates/shared/src/papyrus_runtime.luau)
does not select those methods using the current state. `goto_state` assigns
`__state`; casts use Lua truthiness and `math.floor`.

Build small differential cases for state dispatch, transitions, casts and signed
integer arithmetic from language/compiler references first. Reserve REA for
native scheduling or dispatch questions those sources leave unresolved. Each
case should produce expected/actual values and an ordered event trace.

## First pilot

Time-box the initial investigation to 90 minutes after prerequisites are ready.
Limit it to the unset-race resolver and one speed consumer.

1. Identify the exact game version, executable SHA-256, load order and pinned
   reference-source revisions. Find function seeds through version-matched
   CommonLib relocations/Address Library or a bounded string/reference search.
   Keep file offsets, image addresses, RVAs and
   runtime addresses distinct; do not reuse addresses from another executable.
2. Check provider readiness and perform one cold import. Record import duration
   and result. Stop on the provider deadline; repeated imports do not resolve a
   resource limit. If the target exceeds this boundary, evaluate an existing
   persistent Ghidra/IDA workflow separately.
3. In one MCP session, follow the two function seeds using function dossiers,
   assembly, references and resolved calls. Verify important decompiler
   interpretations against instructions. Record unresolved indirect calls.
4. Export a private evidence bundle. Write a behavior specification that cites
   the binary hash, reference revisions, addresses and evidence IDs, and labels
   observations, inferences and unknowns.
5. Validate the proposed behavior separately in retail Windows Skyrim with
   position/time measurements. Introduce independently authored synthetic
   Rust/Luau regression cases once the rule is supported.

The static pilot succeeds if it identifies the unset-race selection path and one
speed consumer with instruction-backed evidence. An unresolved result is useful
if it states the exact missing branch, address mapping or runtime observation.
It does not justify changing movement constants.

Initial CLI checks, once the toolchain and a small source-owned PE fixture are
available:

```sh
export GHIDRA_INSTALL_DIR=/absolute/path/to/ghidra_12.1.4_PUBLIC
export JAVA_HOME=/absolute/path/to/full-jdk
npx -y rea-agents@6.0.0 doctor --provider ghidra --json
npx -y rea-agents@6.0.0 inspect /absolute/path/to/source-owned-x64-pe.exe --provider ghidra --json
```

These are proposed commands; they were not executed during this assessment.
After that smoke test, connect the pinned package's `mcp` command with the same
provider environment and use its advertised schemas. Open `SkyrimSE.exe` once
and make focused queries in that session:

```json
{"tool":"open_binary","arguments":{"path":"/absolute/path/to/SkyrimSE.exe","provider_id":"ghidra"}}
{"tool":"read_function_instructions","arguments":{"procedure":"0x<version-matched-function-VA>"}}
{"tool":"address_to_file_offset","arguments":{"address":"0x<version-matched-function-VA>"}}
{"tool":"analyze_function","arguments":{"procedure":"0x<version-matched-function-VA>"}}
{"tool":"export_evidence_bundle","arguments":{"path":"/absolute/path/to/private-research/movement-evidence.json"}}
{"tool":"close_binary","arguments":{}}
```

These wrappers illustrate calls; `tool` is not an input field of the named tool.
The address placeholders must be replaced. Set the MCP client's first-call
deadline above the provider's 330 seconds; that is a client setting, not a REA
tool argument. A smaller query still requires the same full cold import.

Keep executables, proprietary assets, shader bytecode and raw game-analysis
bundles local. Repository deliverables should contain behavior notes,
provenance and synthetic fixtures, following [the project's policy](../../LEGAL.md).

## Verification performed

Reviewed REA documentation, provider code and current Mudcrab implementation;
checked the live npm release metadata. The local host is Darwin arm64. `node`,
`npm` and `rea` were absent from the current shell PATH; `java -version` reported
no Java runtime. No Ghidra or Hopper application appeared in `/Applications`.
Other installation locations were not exhaustively searched.

No package installation, agent configuration change, native game analysis or
game execution was performed. This assessment establishes a source-based fit
and a pilot plan; it does not demonstrate successful Skyrim decompilation.

[rea-readme]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/README.md
[npm]: https://registry.npmjs.org/rea-agents/latest
[release]: https://github.com/morluto/rea/releases/tag/rea-agents-6.0.0
[installation]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/docs/installation.md
[windows]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/docs/windows-ghidra-p0.md
[native]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/docs/native-investigation.md
[cli]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/docs/cli.md
[contracts]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/docs/mcp-contracts.md
[provider]: https://github.com/morluto/rea/blob/be783efaba62dc76c5d8d42229869a32afdb302a/src/ghidra/GhidraProvider.ts
[community-shaders]: https://github.com/doodlum/skyrim-community-shaders/blob/2f2919a71bed6132b125e41781304c8f6f73d002/package/Shaders/Lighting.hlsl
