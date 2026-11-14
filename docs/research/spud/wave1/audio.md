# Audio: spud sound, music, ambience and HRTF

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Spud `src/audio`: descriptor playback, music, ambience, reverb, speech and headphone spatialisation.

## Summary
- Spud imports Skyrim audio descriptors and converted assets, then runs sound, music, ambience, reverb, and optional headphone spatialization.
- Scope is 24 files and 7,160 lines, measured with `find` and `wc -l`; `docs/AUDIO.md` documents substantial coverage.
- Maturity is high for the documented audio pipeline; engine integration and asset parity remain distinct from source implementation.
- Key features: descriptor playback and 3D attenuation; audio-thread command system; music heap/transitions; acoustic spaces/weather/region sounds; HRTF renderer using SADIE II data.
- Also supports voice playback, speech ducking, footstep/impact/collision selection, and capture tooling.
- Largest liberties: optional HRTF changes vanilla output; binaural model uses a generic KU100 head; audio updates at a 33 ms cadence.
- Decoding re-encodes xWMA/FUZ to Ogg; this differs from the game's original runtime path.
- Worth borrowing: descriptor-oriented setup, separate audio thread, and measured HRTF validation methodology.

## Feature map
| id | feature | what vanilla Skyrim does (1 line) | how spud does it (approach, algorithm, key types) | where (file:line, symbols) | evidence | liberties | worth borrowing |
|---|---|---|---|---|---|---|---|
| audio-engine | Device and mixer | Mixes game voices through XAudio2 output and reverb graph. | XAudio2 device builds mastering voice and reverb submix; audio thread dispatches commands. | `src/audio/device.hpp`; [`src/audio/system.hpp:51`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.hpp#L51) `AudioSystem`; `docs/AUDIO.md` Threads | impl | engine timing/voice graph approximates game | yes, thread ownership boundary |
| audio-descriptors | Sound descriptors | Resolves sound records, category, output model, attenuation and flags. | `GameSound` receives `SoundSetup`, applies volume, pitch, loops, envelopes and output model. | [`src/audio/sound.hpp:34-50`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.hpp#L34-L50); [`src/audio/sound.cpp:208`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L208) | impl | details depend on imported `.osd` records | yes, descriptor setup object |
| audio-3d | 3D positioning and attenuation | Positions voices and applies distance curves, filtering and speaker panning. | X3DAudio positions sources, applies output model curves, low-pass and reverb sends. | [`src/audio/sound.cpp:33-45`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L33-L45), [`:337-428`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L337-L428) | impl | 33 ms audio pass may differ in responsiveness | yes, keep game spatial rules isolated |
| audio-streaming | Clip decode/cache/stream | Decodes supported game audio into voices, with streaming for long assets. | WAV and Vorbis clip paths, loader thread and 256 MB cache; music/voices stream. | [`src/audio/clip.cpp:92`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L92) `VorbisStream`; [`:136`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L136) `WaveStream`; `docs/AUDIO.md` Threads | impl | xWMA/FUZ assets are converted to Ogg at import | yes, bounded cache |
| audio-music | Music rules | Selects tracks by music type, conditions, priority and transitions. | `MusicManager` manages heap/types/tracks/status and transitions/ducking. | [`src/audio/music.hpp:14`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.hpp#L14); [`src/audio/music.cpp:26`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L26); `docs/AUDIO.md` hooks | impl | subset behavior depends on imported records and engine events | yes, explicit state/status model |
| audio-acoustic-spaces | Reverb/acoustic spaces | Selects reverb from interior/cell acoustic settings. | Environment selects acoustic state and XAudio2 reverb submix settings. | [`src/audio/ambience.hpp:19`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.hpp#L19) `Ambience`; `src/audio/device.*`; `docs/AUDIO.md` | impl | exact room propagation/occlusion not established | maybe, environment snapshot |
| audio-ambience-weather | Region/weather/water sounds | Plays conditional ambience tied to regions, weather, water and emitters. | Ambience evaluates imported conditions and manages region/weather/water/placed loops. | `src/audio/ambience.cpp`; `src/audio/conditions.cpp`; `docs/AUDIO.md` Pieces | impl | no claim of full environmental propagation | yes, keep rule evaluation centralized |
| audio-voice | Dialogue/NPC voice | Plays dialogue and ambient NPC lines with category and spatial rules. | `play_file`/`speak` APIs; voice category and distance-based speech highlight ducking. | [`src/audio/system.hpp:51`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.hpp#L51); `docs/AUDIO.md` Hooks | partial | dialogue choice and lip timing explicitly outside audio area | maybe, caller-owned line selection |
| audio-lipsync | Lip-sync data | Dialogue lip files animate facial motion during speech. | Import converts FUZ lip data to `.osv`; playback/lip driving belongs to other system. | `docs/AUDIO.md` importer and scope; code importer outside audio | partial | audio area does not animate faces | maybe, separate synchronized data channel |
| audio-hrtf | Headphone HRTF | Vanilla uses X3DAudio speaker panning; its HRTF label is not binaural filtering. | Optional XAPO binaural voice filters interpolate four nearest SADIE directions, 64 taps. | [`src/audio/hrtf.hpp:21`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/hrtf.hpp#L21); [`src/audio/binaural.hpp:24`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.hpp#L24); `docs/AUDIO.md` Headphone spatial audio | impl | intentional audible improvement, not vanilla parity | yes, measured filter interpolation |
| audio-sadiedata | HRTF data pipeline | No equivalent per-user generic KU100 binaural filter set in vanilla mix. | D1 KU100 SADIE II 9,201 measurements reduced to 306 directions via tool into `hrtf_data.inc`. | `src/audio/hrtf_data.inc`; `docs/AUDIO.md` The data | impl | generic dummy head can cause front/back confusion; Apache-2.0 data license documented | yes, data provenance and measurements |
| audio-events | Footsteps, impacts, collisions, animation sound events | Selects material/race/gait sound and responds to animation/game events. | `Ambience` selects sets; system receives animation annotations and collision/material callbacks. | [`src/audio/ambience.hpp:19`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.hpp#L19); `docs/AUDIO.md` Hooks | impl | event timing is owned by animation/physics integration | yes, event API per semantic type |
| audio-capture | Capture and scripted diagnostics | Engine exposes no comparable player-facing capture interface. | XAPO capture tap and script commands capture six-channel float WAV and dump state. | `src/audio/capture.hpp`; [`src/audio/capture.cpp:36`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L36); `docs/AUDIO.md` Scripts | impl | tooling feature beyond vanilla gameplay | yes, deterministic audio state snapshots |

## Data and file formats
- Imported descriptor database: `audio/sound.osd` (`docs/formats/osd.md`).
- Converted source files: WAV copied; xWMA and FUZ audio decoded/re-encoded as Ogg Vorbis; lip data stored as `.osv` (`docs/formats/osv.md`).
- HRTF filters are compiled into `src/audio/hrtf_data.inc`; build tool is `tools/hrtf/build_hrtf.py`; source is SADIE II D1 data.
- Captures are float WAV; settings persist in `settings.ini` through engine settings integration.

## Cross-area dependencies
- Importer provides `.osd`, audio assets, conditions and converted lip streams.
- Engine sends listener/environment, menu/dialogue state, sound requests, actor speech and settings.
- Animation emits footstep and sound annotations; physics provides material contacts and impact speed.
- Exposes playback, music, environment, category controls, state snapshots and capture.

## Tests, tooling and gates
- `docs/AUDIO.md` records measured headless comparisons for level, ITD/ILD, height spectrum, rotation clicks, dialogue attenuation and device rates.
- The doc names headless `audio capture`, `audio state`, `audio hrtf`, environment and event script commands. None were run for this audit.
- No dedicated audio test target was established from this read-only source pass.

## Liberties and deviations
1. **Visible:** optional HRTF is deliberately an upgrade to the vanilla mix, with altered spatial cues and near-field gain. Evidence: `docs/AUDIO.md` Headphone spatial audio and measurements; [`src/audio/binaural.cpp:37`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L37).
2. **Visible/internal:** generic KU100 filters cannot match every listener's head and may cause front/back confusion. Evidence: `docs/AUDIO.md` data and limitations.
3. **Internal/compatibility:** import converts xWMA/FUZ to Ogg Vorbis and lip data to `.osv`, rather than preserving original runtime codec path. Evidence: `docs/AUDIO.md` importer; `src/audio/clip.cpp` stream implementations.
4. **Potential gameplay correctness:** scheduled audio pass is 33 ms and some sound polling up to 66 ms per source, which may shift timing. Evidence: `docs/AUDIO.md` Threads; [`src/audio/sound.cpp:501`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L501).

## Claims versus code
- AUDIO describes broad Skyrim rule fidelity; code has descriptor, music, ambience, spatialization and event components matching those claims.
- Lip data is converted, but lip animation is explicitly outside audio; do not count conversion as facial runtime.
- HRTF claims include specific measurement outcomes in docs; this audit did not reproduce those experiments.
- SADIE data provenance is documented; compiled data file presence is confirmed, but build-tool output was not regenerated.

## Open questions
- Which importer records/conditions are fully mapped? Cheapest check: compare `audio_import.cpp` field coverage against OSD schema and fixtures.
- Are music transition and ambience rules covered by tests? Cheapest check: inspect repository test registration beyond this scoped source pass.
- Does lip playback synchronize with facial animation in the engine? Cheapest check: trace `.osv` consumers and dialogue call sites.
