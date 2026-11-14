# Wave 2: audio ingestion and platform-neutral interface (spud)

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Wave 2: audio ingestion and platform-neutral interface (spud)

Spud reference: commit 8c990420, read-only. All line numbers are from that commit. Spud is MIT; Mudcrab is
GPL-3.0. This report records ideas and measured behaviour only; no code was copied.

## Scope read

Files read in full (line counts measured with `wc -l`):

| File | Lines | What it gave |
|---|---|---|
| `src/importer/convert/audio_import.cpp` | 1287 | Every audio record import step, INI defaults, emitter extraction, file conversion loop |
| `src/importer/convert/audio_import.hpp` | 47 | Import result types, `audio_cache_path`, `convert_audio_files` |
| `src/importer/convert/audio_codec.cpp` | 421 | xWMA parse, Media Foundation decode, FUZ split, LIP decode, Vorbis encode, quality rule |
| `src/importer/convert/audio_codec.hpp` | 47 | `PcmClip`, `FuzParts`, `decode_xwma`, `encode_vorbis` |
| `src/formats/osd.hpp` | 455 (1-300 read closely) | `.osd` schema: 28 chunk structs, flags, units |
| `docs/formats/osd.md` | 68 | Chunk table and semantics of `sound.osd` |
| `docs/formats/audio.md` | 24 | Cache file policy, Vorbis settings, determinism |
| `docs/AUDIO.md` | 161 | Threads, hooks, settings, HRTF measurements, script commands |
| `src/audio/system.hpp` | 194 | Public API surface (the whole backend boundary) |
| `src/audio/system.cpp` | 1487 (sections 60-945, 1114-1487 read) | Loader thread, `start_sound`, `setup_for`, commands, `tick`, `run`, `animation_sound`, speech, cache |
| `src/audio/internal.hpp` | 261 | `PlayRequest`, `Fade`, `LiveSound`, `Command`, `CommandRing`, `Impl` state |
| `src/audio/sound.hpp` | 153 | `SoundSetup`, `GameSound`, `CategoryState`, `Listener` |
| `src/audio/sound.cpp` | 565 (1-200, 280-565 read) | Voice creation, buffer submit, 3D matrix, volume formula, culling, loop handling |
| `src/audio/clip.hpp` | 47 | `Clip`, `ClipInfo`, `ClipStream` |
| `src/audio/clip.cpp` | 193 | RIFF parse, `smpl` loop, stb_vorbis stream, `load_clip` |
| `src/audio/music.hpp` | 34 | `MusicManager` interface |
| `src/audio/music.cpp` | 697 (28-700 read) | Track/type/heap rules, palettes, cues, ducking |
| `src/audio/ambience.hpp` | 41 | `Ambience` interface |
| `src/audio/ambience.cpp` | 1015 (28-1015 read) | Spaces, reverb, cells, regions, weather, water, emitters, footsteps, impacts |
| `src/audio/device.hpp` | 120 | Graph layout, voice handles, reverb transition, HRTF toggles |
| `src/audio/device.cpp` | 224 (30-224 read) | XAudio2 device creation, mastering voice, reverb submix, source voices |
| `src/audio/hrtf.hpp` | 86 | `HrtfSet`, `BinauralRenderer` state |
| `src/audio/hrtf_data.inc` | 1278 (header, ring table, ITD table head) | 306 directions, 64 taps, 48 kHz, SADIE II notice |
| `src/audio/binaural.hpp` | 42 | HRTF submix voice and XAPO |
| `src/audio/binaural.cpp` | 97 | `BinauralXapo` process, submix matrix to FL/FR |
| `src/audio/capture.hpp` | 60 | Capture tap API |
| `src/audio/capture.cpp` | 174 (30-80 read) | Capture XAPO, float WAV header |
| `src/audio/conditions.cpp` | 72 | Six condition functions, OR-group evaluation |
| `src/audio/CMakeLists.txt` | 6 | XAudio2/xapobase linked on Windows only |

Not read, and why:
- `src/audio/hrtf.cpp` (275) read only through `rg` line hits: ring build, `resample`, `lookup`, `BinauralRenderer::process` boundaries. The filter maths detail was not line-by-line.
- `src/audio/stb_vorbis_impl.cpp` (12): `#define STB_VORBIS_HEADER_ONLY`/implementation include only.
- `tools/hrtf/build_hrtf.py`: only the first 8 lines of `tools/hrtf/LICENSE-SADIE-II.txt` (Apache-2.0 text).
- Producer call sites (`src/engine/app_*.cpp`) by `rg` counts and `app_host.cpp` 786-1110 only, not whole files.
- `docs/formats/osv.md`, `apps/` scenario launchers, `src/engine/user_settings.*`: out of the audio scope.

## Architecture

### End to end

```
Data/ (BSA + loose)
   |  audio_import.cpp Builder::run()
   |    26 record signatures -> sound.osd (chunks CATG OMDL COND DESC FILS SOUN REVB ASPC MUSC
   |    MUST TRKL CUES FSTP FSTS FSTL IPCT IPDS IPPR MATT REGN RSND RARA RPTS WATR DOBJ WTHR
   |    WSND WRLD LCTN CELL CREG EMIT SETT STRS)
   |  convert_audio_files(): .wav copy, .xwm/.fuz -> .ogg, .fuz/.lip -> .osv
   v
cache/audio/sound.osd  +  cache/sound/**.wav|.ogg  +  cache/music/**.ogg  +  .osv
   |
   v  AudioSystem ctor: read sound.osd into m_db, index names, start Loader + audio thread
main thread                      audio thread (33 ms)                loader thread
play()/stop()/set_position()  -> CommandRing(4096) -> Impl::tick -> Loader::request
set_listener()/set_environment() ->                      start_sound/setup_for     load_clip/open_stream
music_add()/footstep()/...                          music->update, ambience->update   256 MB LRU
                                                          |
                                                          v
                                          GameSound -> IXAudio2SourceVoice --send--> mastering (6 ch)
                                                                          \--send--> reverb submix (2 ch, I3DL2)
                                                    optional: -> BinauralVoice submix (XAPO HRTF) -> FL/FR
```

### Dataflow from Skyrim records to the runtime

1. Import reads the winning version of each record and writes indices, not FormIDs, for the tables it owns
   (`k_none` = `0xFFFFFFFF`) ([`docs/formats/osd.md:8-12`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L8-L12)).
2. Shape of each family:
   - `SNCT` -> category: parent index, flags, `VNAM/65535`, `UNAM/65535`
     ([`audio_import.cpp:316-333`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L316-L333), [`osd.hpp:31-40`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L31-L40)).
   - `SOPM` -> output model: `NAM1` byte 0 flags + byte 3 reverb send/100, else legacy `FNAM` with send 0.5;
     `MNAM` output type; speaker levels default from a 3x8 table then `ONAM` (24 bytes) or `SNAM`
     (8/16 bytes); `ANAM` gives min/max distance (converted to metres) and 5 curve bytes
     ([`audio_import.cpp:335-380`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L335-L380)).
   - `SNDR` -> descriptor: `GNAM` category, `ONAM` output model, all `ANAM` files (each through the
     extension fallback), `LNAM` flags (else `FNAM` with three bit remaps 0x10->0x800,
     0x2000000->0x1000, 0x4000000->0x2000), `BNAM` five fields, then `CTDA` conditions; `SNAM`
     alternate is resolved in a second pass ([`audio_import.cpp:382-435`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L382-L435)).
   - `SOUN` -> marker -> descriptor ([`audio_import.cpp:437-445`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L437-L445)). Any "sound" field accepts SNDR or, on old
     form versions, SOUN (`sound_or_marker`, [`:447-455`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L447-L455)).
   - `REVB` -> 14 bytes of `DATA` copied with units kept as stored ([`audio_import.cpp:457-480`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L457-L480)).
   - `ASPC` -> ambient descriptor, `RDAT` region FormID, `BNAM` reverb, `OBND` bounds in metres
     ([`audio_import.cpp:482-503`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L482-L503)).
   - `MUST` -> track: type from `CNAM` GUID (0x23F678C3 palette, 0xA1A9C4D5 silent, else single), `FLTV`
     duration, `DNAM` fade-out, `ANAM`/`BNAM` files, `LNAM` loop, `FNAM` cue points, `SNAM` palette
     sub-tracks ([`audio_import.cpp:505-570`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L505-L570)).
   - `MUSC` -> type: flags, `PNAM` priority+ducking, `WNAM` fade, `TNAM` track list
     ([`audio_import.cpp:570-587`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L570-L587)).
   - `MATT`/`IPCT`/`IPDS` -> material parent + `HNAM` impact set; impact `DATA` (16 or 24 bytes), `SNAM`/`NAM1`
     sounds; pairs from `PNAM` (material, impact) ([`audio_import.cpp:589-647`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L589-L647)).
   - `FSTP`/`FSTS` -> tag + impact set; `XCNT` gait counts with `DATA` lists stored in reverse gait order
     ([`audio_import.cpp:649-687`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L649-L687)).
   - `REGN` -> `WNAM` world, `RPLD` polygons (metres), `RDAT` type 7 entry with override/priority, `RDMO`
     music, `RDSA` sounds with 4 weather flags and chance ([`audio_import.cpp:689-736`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L689-L736)).
   - `WTHR` -> 20 `DATA` bytes and `SNAM` (descriptor, type) ([`audio_import.cpp:738-760`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L738-L760)).
   - `LCTN`/`WRLD` -> parent + music; world also water ([`audio_import.cpp:762-776`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L762-L776)).
   - `CELL` -> flags, water + `XCLW` sentinel rules, grid, `XCAS`, `XCMO`, location via `XEZN` else `XLCN`,
     `XCLR` regions, and every `REFR` child that makes an emitter ([`audio_import.cpp:871-991`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L871-L991)).
   - `REFR` -> emitter: SOUN/ASPC base, or ACTI/MSTT/LIGH/DOOR with a `SNAM` (DOOR `BNAM`) descriptor; position,
     rotation quaternion, `XSCL`, `XPRM` bounds/type, line endpoints, initial enable state
     ([`audio_import.cpp:802-870`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L802-L870), [`:1041-1079`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1041-L1079)).
   - `DOBJ` -> merged `{signature, FormID}` map over all plugin versions ([`audio_import.cpp:1005-1039`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1005-L1039)).
   - INI -> 42 defaults overridden by `Skyrim_Default.ini`, stored as `"name:Section"` settings
     ([`audio_import.cpp:111-157`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L111-L157), [`:1080-1115`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1080-L1115)).
3. Runtime resolution: descriptor -> category -> parent chain; descriptor -> output model -> attenuation curve,
   speaker levels, reverb send; emitter -> cell -> region list; cell -> music via XCMO, location parents,
   region music, world music ([`ambience.cpp:328-372`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L328-L372)).
4. Playback: `start_sound` evaluates the descriptor's conditions once with the player as subject
   ([`system.cpp:445-455`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L445-L455)), picks a file with a no-repeat re-roll ([`system.cpp:225-234`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L225-L234)), computes frequency and
   static attenuation ([`:243-252`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L243-L252)), then hands the file to the loader ([`:521`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L521)).

### Threading and ownership

- Main thread: `play`, `stop`, `set_position`, `music_add`, `footstep`, `set_listener`, `set_environment` only
  push a `Command` into a 4096-entry SPSC ring that never blocks ([`internal.hpp:99-124`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/internal.hpp#L99-L124), [`system.cpp:1114-1250`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L1114-L1250)).
- Audio thread: owns `Impl`, `MusicManager`, `Ambience`, every `GameSound` and the `Device`. It runs
  `tick(dt)` then sleeps `uAudioThreadSleepTimeGameMode` (33 ms; 1 ms in menus) ([`system.cpp:913-938`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L913-L938)).
  Cadence is therefore 33 ms for fades/music/ambience and at least 66 ms for the 3D/voice poll
  ([`sound.cpp:501-503`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L501-L503)).
- Loader thread: one `std::jthread` with a mutex+condition variable queue, a weak_ptr cache and a 256 MB
  "recent" deque; it decodes whole clips or opens a stream above the 4 MiB threshold ([`system.cpp:80-130`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L80-L130),
  [`internal.hpp:127-151`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/internal.hpp#L127-L151)).
- XAudio2 thread: owns mixing, runs `GameSound::Callback` and both XAPOs, and drives the reverb transition
  ([`device.cpp:162-202`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L162-L202), [`binaural.cpp:46-57`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L46-L57), [`capture.cpp:40-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L40-L68)).
- Shutdown order: audio thread joined in `~Impl`, device and voices released in `run` after `sounds.clear()`
  ([`system.cpp:163-168`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L163-L168), [`:934-937`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L934-L937), [`device.cpp:98-113`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L98-L113)).

### Reference scale

- 23 source files plus its `CMakeLists.txt`: 7154 lines including `hrtf_data.inc` (4811 `.cpp`, 1065 headers,
  1278 generated data, measured with `wc -l`).
- Importer side: 1802 lines across four files.
- One backend: XAudio2 2.9 + X3DAudio + XAPO, Windows only ([`README.md:11`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/README.md#L11), [`src/audio/CMakeLists.txt:4-6`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/CMakeLists.txt#L4-L6)).

## Feature ledger

Evidence codes: `impl+tests` (a test was found that exercises it), `impl`, `partial`, `stub`, `doc-only`,
`unverified`. Vanilla rules are the ones spud's comments cite, marked "(cited)" when the comment names the
game routine; `unknown` where the report could not establish the rule.

### A. Ingestion: records

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| audio-import-record-set | Which record families become the sound database | Full audio families incl. REVB, ASPC, MUSC, MUST, FSTP, IPCT (cited) | 26 signatures compiled into 34 `.osd` chunks (28 structs plus shared index arrays) | [`audio_import.cpp:36-61`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L36-L61), [`:208-228`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L208-L228), [`osd.md:14-49`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L14-L49) | impl | Reads only what the runtime consumes; no MUSC palette re-root data or SOPM CNAM | Whole family absent -> empty chunk |
| audio-import-index-identity | Cross-reference encoding | Records link by FormID | Index into chunk array, `0xFFFFFFFF` when absent; MATT/REGN/LCTN/WRLD parent links stay FormIDs | [`osd.md:8-10`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L8-L10), [`audio_import.cpp:245-253`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L245-L253) | impl | Requires a fixed table order at import | Unresolved link -> `k_none`, sound silently skipped |
| audio-import-string-table | String storage | EditorIDs and paths live in the plugin | NUL-terminated `STRS` offsets; empty string becomes `k_no_string` | [`audio_import.cpp:233`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L233), [`osd.md:49`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L49) | impl | none seen | `k_no_string` |
| audio-import-ext-fallback | File reference resolution | Game tries `.wav`, `.fuz`, `.xwm` in that order, ignoring the record's extension (cited) | `path::replace_extension` loop over the three, first existing wins, records the hit in `m_files` | [`audio_import.cpp:256-275`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L256-L275) | impl | none seen | No candidate -> `k_no_string` and `missing[kind]++` |
| audio-import-snct | Sound category | Parent chain, flags, `VNAM` static volume, `UNAM` menu default | Flags, `VNAM/65535`, `UNAM/65535`, parent resolved in a second pass | [`audio_import.cpp:316-333`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L316-L333) | impl | none seen | `UNAM` absent -> 1.0, `VNAM` absent -> 1.0 |
| audio-import-sopm-flags | Output model flags and reverb send | `NAM1` byte 0 flags, byte 3 send percent; pre-35 records use `FNAM` with 50 % send | Same, `reverb_send = byte3/100` | [`audio_import.cpp:339-351`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L339-L351) | impl | Legacy records forced to 0.5 | No NAM1/FNAM -> flags 0, send 0 |
| audio-import-sopm-speakers | Speaker level matrix | 3 rows (mono, stereo L, stereo R) x 8 columns, defaults then `ONAM`/`SNAM` | 3x8 hard-coded default table, `ONAM` >= 24 bytes overwrites all, `SNAM` 8/16 bytes fills rows 0 or 1-2 | [`audio_import.cpp:352-368`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L352-L368), [`osd.hpp:50-62`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L50-L62) | impl | Stereo rows only filled from SNAM when exactly 8 or 16 bytes | Default table |
| audio-import-sopm-attenuation | Distance curve | `ANAM`: min, max, 5 curve percent bytes | Metres conversion, `has_attenuation` set | [`audio_import.cpp:369-379`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L369-L379) | impl | Curve kept as percent; the runtime interpolates it | Absent -> no curve, `attenuates` flag unused |
| audio-import-sndr-flags | Descriptor flag word | `LNAM`; legacy `FNAM` remaps loop/envelope bits | Same three remaps | [`audio_import.cpp:402-415`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L402-L415) | impl | none seen | No flags -> 0 |
| audio-import-sndr-params | Frequency, priority, static attenuation | `BNAM`: shift ±%, variance %, priority, dB variance, static attenuation in 0.01 dB | Same fields, raw units kept | [`audio_import.cpp:416-423`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L416-L423), [`osd.hpp:91-108`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L91-L108) | impl | none seen | `BNAM` absent -> 0 shift, 100 % base |
| audio-import-sndr-alternate | "Alternate sound for" | `SNAM` names a descriptor that replaces another while the first loads | Second pass resolves the index | [`audio_import.cpp:384-386`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L384-L386), [`:427-433`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L427-L433) | impl | none seen | Absent -> `k_none` |
| audio-import-ctda | Conditions on descriptors, tracks | `CTDA` with comparison, function, params, run-on, reference | 32-byte struct copied; global comparison resolved; reference resolved | [`audio_import.cpp:277-314`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L277-L314), [`osd.hpp:68-82`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L68-L82) | impl | `run_on` stored but never read by the evaluator | Malformed (<32 bytes) skipped |
| audio-import-ctda-formparams | Form parameters in conditions | 8 functions take a form parameter | Table of 8 function ids with param1/param2 form flags, resolved to load-order FormIDs | [`audio_import.cpp:159-175`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L159-L175), [`:301-309`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L301-L309) | impl | Only 8 functions covered; others keep raw values | Unknown function -> raw value |
| audio-import-soun | Sound marker | `SOUN` `SDSC` -> descriptor | Marker table, plus `sound_or_marker` for old records | [`audio_import.cpp:437-455`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L437-L455) | impl | none seen | Missing descriptor -> `k_none` |
| audio-import-revb | Reverb record | `DATA` 14 bytes: decay ms, HF ref, room/HF filters, reflections, amp, ratios, delays, diffusion, density | Copied as stored with documented scale factors | [`audio_import.cpp:457-480`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L457-L480), [`osd.hpp:120-136`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L120-L136) | impl | Units are not normalised at import | Short DATA -> record with zeros |
| audio-import-aspc | Acoustic space | Ambient loop `SNAM`, `RDAT` region, `BNAM` reverb, `OBND` bounds | Same, bounds in metres | [`audio_import.cpp:482-503`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L482-L503) | impl | none seen | Missing parts -> `k_none` |
| audio-import-must | Music track | Type from `CNAM` GUID, duration `FLTV`, fade `DNAM`, files `ANAM`/`BNAM`, loop `LNAM`, cues `FNAM`, palette `SNAM` | Same; cue floats appended in file order | [`audio_import.cpp:507-559`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L507-L559) | impl | Cue points not sorted at import (sorted at use) | Missing file -> `k_no_string` |
| audio-import-musc | Music type | `FNAM` flags, `PNAM` priority+ducking, `WNAM` fade, `TNAM` tracks | Same; ducking in 0.01 dB | [`audio_import.cpp:570-587`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L570-L587) | impl | none seen | No `TNAM` -> zero tracks |
| audio-import-palette-split | Palette layers | `SNAM` empty entries split up to three layers | Import keeps `k_none` entries; the manager splits them | [`music.cpp:71-88`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L71-L88), [`osd.md:26`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L26) | impl | Splitting done at runtime, not import | `k_none` preserved |
| audio-import-matt | Material | Parent, flags, `HNAM` Havok impact set, `MNAM` name | Same, parent and set resolved in a second pass | [`audio_import.cpp:589-627`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L589-L627) | impl | none seen | Missing `HNAM` -> `k_none` |
| audio-import-ipct | Impact | `DATA` 16 or 24 bytes, `SNAM`/`NAM1` sounds | Fields read when 24 bytes; placement radius to metres | [`audio_import.cpp:600-620`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L600-L620) | impl | 16-byte DATA leaves sound_level/flags/result 0 | Absent sounds -> `k_none` |
| audio-import-ipds | Impact set | `PNAM` pairs (material, impact) | Pair table, resolved | [`audio_import.cpp:621-638`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L621-L638) | impl | none seen | Pair with unknown impact -> `k_none` |
| audio-import-fsts-order | Footstep set gait order | `XCNT` counts are walk..swim; `DATA` lists are reversed (swim..walk) (cited xEdit) | Loop fills gaits from 4 down to 0 | [`audio_import.cpp:663-680`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L663-L680) | impl | Relies on the documented reverse order | Missing `DATA` -> empty gait lists |
| audio-import-regn | Region sounds | `RDAT` type 7 entry, override/priority, `RDMO` music, `RDSA` sounds with 4 flags + chance, `RPLD` polygons | Same; `entry_type` gates later subrecords | [`audio_import.cpp:689-736`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L689-L736) | impl | Only `RDAT` type 7 handled; other region data types ignored | No `RDAT` -> `has_sounds` 0 |
| audio-import-wthr | Weather sounds | `DATA` 20 bytes, `SNAM` (descriptor, type) | Copied; type enum from the second u32 | [`audio_import.cpp:738-760`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L738-L760) | impl | `DATA` bytes reinterpreted by the runtime (index 6-11) | Missing -> no sounds |
| audio-import-watr | Water record | `SNAM` open sound, `FNAM` flags | Same | [`audio_import.cpp:993-1003`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L993-L1003) | impl | none seen | No `SNAM` -> no water ambience |
| audio-import-lctn-wrld | Location and world music | `LCTN` `NAM1`, `WRLD` `ZNAM`, water | Same | [`audio_import.cpp:762-776`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L762-L776) | impl | World music only used after region music | Absent -> `k_none` |
| audio-import-cell | Cell audio fields | `XCAS`, `XCMO`, `XLCN`/`XEZN`, `XCLR`, `XCWT`, `XCLC` grid | All read; location prefers the encounter zone's location | [`audio_import.cpp:878-931`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L878-L931) | impl | Interiors get `world = 0` | Missing -> `k_none` |
| audio-import-cell-water | Water height sentinels | Huge value = worldspace default; ±1e9 = no water (cited) | `>= 3e38` keeps default, `\|h\| >= 1e9` clears the flag | [`audio_import.cpp:897-909`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L897-L909) | impl | none seen | No `XCLW` -> world default |
| audio-import-emitter-kinds | Placed sound emitters | Game handles SOUN, ACTI, DOOR, LIGH, MSTT; not TACT (cited) | Same five kinds, `SNAM` descriptor (DOOR `BNAM`), kind enum | [`audio_import.cpp:802-836`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L802-L836) | impl | Only emitters with a descriptor are kept | No descriptor -> emitter dropped |
| audio-import-emitter-line | Line sound markers | `XPRM` type 4: half-extent rotated as a row vector, slide from `+v` to `-v` with the Y sign flipped | 3x3 matrix built from `Rx(-x) Ry(-y) Rz(-z)`, `b·M`, `(vx,-vy,vz)` | [`audio_import.cpp:1041-1079`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1041-L1079) | impl | none seen | No `XPRM` -> point emitter |
| audio-import-emitter-enable | Initial enable state | Reference disabled now, or through an `XESP` parent chain with the opposite flag | Recursive `enabled()` with depth 64 and a memo | [`audio_import.cpp:777-801`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L777-L801), [`:862`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L862) | impl | Depth capped at 64; the player ref is always enabled | Cycle -> treated as enabled |
| audio-import-dobj | Default objects | `DOBJ` merges every plugin's pairs; later ones override per signature (cited) | All versions walked, 4-char signature, resolved FormID | [`audio_import.cpp:1005-1039`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1005-L1039) | impl | none seen | Unlisted signature -> 0 |
| audio-import-settings | INI and game settings | The audio `[Audio]`/`[AudioMenu]`/`[General]`/`[Weather]` values the rules read | 42 compiled defaults, overridden from `Skyrim_Default.ini` by `name:Section` | [`audio_import.cpp:111-157`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L111-L157), [`:1080-1115`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1080-L1115) | impl | Only `Skyrim_Default.ini`, not the user's `Skyrim.ini` | Missing key -> compiled default |
| audio-import-persistent-refr | Persistent-cell references | Exterior persistent refs belong to the cell under them | Grid computation from position / 4096 units and reassignment; orphans counted and dropped | [`audio_import.cpp:949-966`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L949-L966) | impl | Orphans are logged, not attached | Outside any cell -> dropped |
| audio-import-missing-counts | Import diagnostics | n/a | `missing[kind]` and `counts[kind]` maps returned to the caller | [`audio_import.cpp:22-23`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L22-L23), [`:18-24`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L18-L24) | impl | Counts are per record family, not per field | Absent -> 0 |
| audio-import-modes | Import entry points | n/a | `--audio-only` and `--no-audio` around the same importer | [`docs/AUDIO.md:22-25`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/AUDIO.md#L22-L25) | doc-only | The flags live in `skytool`, not in this scope | Flag absent -> audio imported |

### B. Ingestion: codecs and files

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| audio-codec-wav-copy | WAV assets | Game plays 16-bit PCM WAV with a `smpl` loop | Byte-for-byte copy, including `smpl`; a non-PCM16 WAV is copied anyway and logged per folder | [`audio_import.cpp:1245-1253`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1245-L1253), [`:1273-1275`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1273-L1275) | impl | Non-PCM16 files pass into the cache and fail at load | Copy failure -> `failed++` |
| audio-codec-wav-probe | WAV PCM16 check | n/a | RIFF walk for `fmt ` and `data`, rejects tag != 1, bits != 16, channels 0 | [`audio_import.cpp:1127-1171`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1127-L1171) | impl | Does not check `block_align` here (clip.cpp does) | Returns false |
| audio-codec-xwma-parse | xWMA container | RIFF `XWMA` with `fmt `, `dpds`, `data` | Chunk walk; validates channels, rate, block align | [`audio_codec.cpp:47-89`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L47-L89) | impl | none seen | Throws on missing fmt/data |
| audio-codec-xwma-decode | xWMA audio | WMA v2 (0x0161) / v9 (0x0162) in an xWMA container | Media Foundation `CLSID_CWMADecMediaObject`, packet by packet, 0x1F user data for v2 | [`audio_codec.cpp:153-244`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L153-L244) | impl | Windows-only; needs Media Foundation | Non-Windows throws "xWMA decoding needs Windows Media Foundation" ([`:326`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L326)) |
| audio-codec-xwma-trim | Decoded length | Stream length from the `dpds` table | Resize to `dpds.back()/2` samples | [`audio_codec.cpp:328-336`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L328-L336) | impl | none seen | Empty `dpds` -> untrimmed |
| audio-codec-fuz-split | FUZ container | `FUZE` + version + lip size + lip + audio | Same; audio span handed to the xWMA or WAV path | [`audio_codec.cpp:248-259`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L248-L259) | impl | none seen | Bad magic throws |
| audio-codec-lip-decode | Lip data | LIP version 1, optional zero-run coding, frame table | Zero-run expansion with a size cap, 16 or 17 modifiers per frame, frame count bounded 1..1e6 | [`audio_codec.cpp:261-306`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L261-L306) | impl | Frames must have uniform modifier count | Version != 1 throws |
| audio-codec-vorbis-encode | Re-encode | Game decodes xWMA directly | libvorbis VBR, same rate and channels, 4096-frame chunks | [`audio_codec.cpp:342-420`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L342-L420) | impl | Another lossy generation; the game never transcodes | Encode error -> `failed++` |
| audio-codec-vorbis-quality | Re-encode quality | n/a | At least 0.4, raised until the per-channel nominal rate is 2x the source | [`audio_codec.cpp:405-421`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L405-L421) | impl | Heuristic from libvorbis stereo tables halved | Above the table -> quality 1.0 |
| audio-codec-determinism | Reproducible output | n/a | Fixed Ogg serial `0x4F534B59` ("SPUD") and a fixed vendor comment | [`audio_codec.cpp:338`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L338), [`:333`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L333) | impl | none seen | n/a |
| audio-codec-skip | Incremental conversion | n/a | Existing target file skipped unless `force`; records are always rewritten | [`audio_import.cpp:1197-1200`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1197-L1200), [`formats/audio.md:22-24`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/audio.md#L22-L24) | impl | No version marker in the cache | Both targets present -> `skipped++` |
| audio-codec-threads | Parallel conversion | n/a | `std::jthread` pool of `threads`, atomic work index, sorted source list | [`audio_import.cpp:1183-1270`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1183-L1270) | impl | Log ordering is by mutex, output is order-independent | `threads` 0 -> one thread |
| audio-codec-cache-path | Cache path mapping | n/a | `.xwm`/`.fuz` -> `.ogg`, everything else unchanged | [`audio_import.cpp:1174-1181`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1174-L1181) | impl | Case is normalised by the data source | `.lip` has no audio output |

### C. Runtime

| id | behaviour | vanilla Skyrim rule | spud implementation | file:line | evidence | deviation | default / no-op |
|---|---|---|---|---|---|---|---|
| audio-runtime-device | Output graph | 6-channel mastering voice + 2-channel reverb submix, XAudio2 2.7/X3DAudio 1.7 (cited) | XAudio2 2.9, 6-channel master, `AudioReverb` submix, `X3DAudioInitialize` on 5.1 and on mono | [`device.cpp:39-95`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L39-L95), [`device.hpp:1-10`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.hpp#L1-L10) | impl | Version difference is claimed measured, not reproduced here | No device -> `available()` false, every call a no-op |
| audio-runtime-rate-clamp | Device rate | Device rate clamped to [20000, 48000] Hz (cited) | Recreates the mastering voice when outside the range | [`device.cpp:45-58`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L45-L58) | impl | none seen | n/a |
| audio-runtime-source-voice | Per-sound voice | One source voice per sound, two filtered sends | `CreateSourceVoice` 16-bit PCM at the clip rate, reverb send zeroed until the first 3D pass | [`device.cpp:115-136`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L115-L136) | impl | Voice creation failure makes the sound keep the default mix ([`sound.cpp:113-118`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L113-L118)) | `nullptr` -> sound finished |
| audio-runtime-command-ring | Non-blocking API | n/a | SPSC ring of 4096 `Command`s, `push` returns false when full | [`internal.hpp:99-124`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/internal.hpp#L99-L124) | impl | A full ring drops the command silently | Full -> command lost |
| audio-runtime-loader | Decode off-thread | n/a | Queue + CV, weak_ptr clip cache, 256 MB recent deque, stream above 4 MiB | [`system.cpp:80-130`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L80-L130) | impl | Cache is per clip size, not per game cache rules | Read error -> sound destroyed, warning |
| audio-runtime-stream-threshold | Streaming | Long files stream, short ones are cached (cited) | Ogg size x10 estimate, threshold `k_stream_threshold` (4 MiB) | [`system.cpp:112-120`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L112-L120) | impl | Constant is an estimate, not the game's `uMaxSizeForCachedSound` | Below -> whole clip in memory |
| audio-runtime-file-choice | File randomisation | Random file, re-rolled once if it repeats the last pick (cited) | `random_below(file_count)`, `+1 % n` when equal to `last_file` | [`system.cpp:225-234`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L225-L234) | impl | Uniform weights; no per-file weights | No files -> `start_sound` returns 0 |
| audio-runtime-frequency | Pitch | `(100 + shift)` clamped to [10, 200] %, plus uniform variance, times the category multiplier | Same formula, `frequency_base` then `±variance` then category chain | [`system.cpp:243-246`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L243-L246), [`:474-482`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L474-L482) | impl | none seen | Variance 0 -> no draw |
| audio-runtime-static-attenuation | Static attenuation | Record value plus uniform dB variance in 0.01 dB (cited) | `atten += random_below(db_variance*100)` | [`system.cpp:248-252`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L248-L252) | impl | none seen | Variance 0 -> no draw |
| audio-runtime-condition-gate | Descriptor conditions | Evaluated once at build with the player as subject (cited) | `evaluate` over the descriptor's span before any file work | [`system.cpp:445-455`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L445-L455) | impl | Only 6 of the game's condition functions return real values ([`conditions.cpp:15-32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/conditions.cpp#L15-L32)) | Any other function -> 0, so most conditions compare to 0 |
| audio-runtime-condition-grammar | Condition grammar | OR groups with AND between groups, comparison in the top 3 bits of byte 0 | `group = group \|\| compare`, group closes on `!OR` | [`conditions.cpp:34-70`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/conditions.cpp#L34-L70) | impl | "Use global" comparison value is read as 0 | Unknown operator -> false |
| audio-runtime-volume | Volume formula | Hundredths of a dB, clamped [-100, 0] dB, distance and fade subtracted (cited) | `mb = log10(v)*2000 - fade - static [- distance]`, clamped, `10^(mb/500)` | [`sound.cpp:458-472`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L458-L472) | impl | none seen | Gain 0 -> -inf clamp at -10000 mB |
| audio-runtime-category-chain | Category hierarchy | Volume and frequency multiply up the parent chain; attenuation adds, capped 10000 | `effective_volume`, `effective_frequency`, `category_attenuation` recurse with depth 32 | [`sound.cpp:185-205`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L185-L205), [`system.cpp:516-525`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L516-L525) | impl | Paused or muted category contributes 4000 mB | Cycle -> stops at depth 32 |
| audio-runtime-category-state | Pause, mute, attenuation broadcast | Changing a category fades every sound in its subtree | `broadcast_category_fade` walks live sounds and starts a fade of at least 0.1 s | [`system.cpp:565-612`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L565-L612) | impl | none seen | Unknown category -> no-op |
| audio-runtime-fade | Fade message | Linear in mB over at least 100 ms, optional stop or pause after | `Fade` struct driven in `tick`, `std::max(duration, 0.1)` applied by callers | [`internal.hpp:44-52`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/internal.hpp#L44-L52), [`system.cpp:854-877`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L854-L877) | impl | none seen | Duration 0 -> 0.1 s minimum |
| audio-runtime-loop-envelope | Looping and envelopes | Loop and envelope flags loop `[loop begin, loop end)` until stopped (cited) | `XAUDIO2_BUFFER` LoopBegin/Length/Infinite; `smpl` chunk or the whole clip | [`sound.cpp:138-161`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L138-L161), [`clip.cpp:56-80`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L56-L80) | impl | Only for whole clips; streamed loops splice in `submit_stream` | Loop end <= begin -> no loop |
| audio-runtime-distance-cull | Distance culling | Attenuating loops without auto-release stop beyond max distance and resume inside it (cited) | `m_culled` with `ExitLoop`, `Stop` or `FlushSourceBuffers+submit` by envelope flag | [`sound.cpp:474-499`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L474-L499) | impl | none seen | No output model -> never culled |
| audio-runtime-3d | 3D positioning | X3DAudio matrix, LPF direct/reverb, reverb send, Doppler (cited) | X3DAudio emitter/listener with the record curve, `CurveDistanceScaler = max distance`, inner radius 69.99 units | [`sound.cpp:351-430`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L351-L430) | impl | Coordinate swizzle `(x, z, y)` into X3DAudio's Y-up frame | No model or no position -> no 3D update |
| audio-runtime-speaker-output | Defined speaker output | Five-point curve on the 5.1 matrix, skipped under one unit of range | Linear interpolation between the five curve points, converted to 0.01 dB | [`sound.cpp:430-456`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L430-L456) | impl | none seen | Range < 1 unit -> no attenuation |
| audio-runtime-default-matrix | File without descriptor | XAudio2's default output matrix, no attenuation (cited) | Flags forced to 2 (no attenuation), output model only when the caller passes one | [`system.cpp:206-221`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L206-L221) | impl | Dialogue relies on the caller's DOBJ output model | No output model -> default matrix |
| audio-runtime-speech-highlight | Speech highlight | Speakers farther than `fMaxHighlightRadius` duck by 12 dB while someone nearer speaks | Two code-side sub-categories of NonVOC, re-assigned each frame, `fNonHighlightSpeechAtten` on the non-highlight parent | [`system.cpp:256-299`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L256-L299), [`:715-733`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L715-L733) | impl | Sub-categories are synthetic indices, not records | No NonVOC category -> highlight disabled |
| audio-runtime-dialogue-duck | Dialogue ducking | NDSC and SFDC duck by `fNonDialogVoiceDucking` over the fade times | Category attenuation set on both DOBJ categories | [`system.cpp:748-763`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L748-L763) | impl | none seen | Missing category -> no-op |
| audio-runtime-menu-pause | Menu and loading pause | PDMC/PIMC pause on menu open, PDLC on the loading screen (cited) | Category pause with `fMenuModeFadeOut/InTime`, plus the menu-mode audio sleep time | [`system.cpp:727-747`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L727-L747), [`:913-938`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L913-L938) | impl | none seen | Missing category -> no-op |
| audio-runtime-animation-sounds | Animation sound events | Handle table per reference and event key; play/stop/release/end/move (cited) | `unordered_map<ref, unordered_map<key, id>>`, range test at the reference, `0x10` follow flags | [`system.cpp:315-359`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L315-L359) | impl | Key is the raw annotation payload string | Non-sound annotation -> `animation_event` returns false ([`:1290-1358`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L1290-L1358)) |
| audio-runtime-sound-cache | Game sound cache model | Until a WAV is cached the game substitutes a cached file of the same or alternate descriptor (cited) | `sound_cache` LRU, `cache_substitute`, `QueueCacheLoad` style background load at priority <= 128 | [`system.cpp:371-443`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L371-L443) | impl | WAV only, as the game streams xWMA (cited) | Cache disabled -> no substitution |
| audio-runtime-music-heap | Music heap | Min-priority heap, equal priorities keep the earlier entry (cited) | Binary heap on the priority byte, `heap_remove` sifts down only | [`music.cpp:513-556`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L513-L556) | impl | Known game quirk reproduced deliberately | Empty heap -> `NoMusic` state |
| audio-runtime-music-track | Track types | Single, palette, silent | Status machine per track with `main`/`finale` handles | [`music.cpp:137-181`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L137-L181), [`:201-273`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L201-L273) | impl | none seen | No file -> track finishes at once |
| audio-runtime-music-palette | Palette layers | Up to three layers, each picking a sub-track, never repeating the previous | `palette_pick` per layer with a no-repeat rule | [`music.cpp:71-88`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L71-L88), [`:182-199`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L182-L199) | impl | Random choice is uniform | No playable sub-track -> layer silent |
| audio-runtime-music-cues | Cue points and finale | Finish toward the next cue, cross-fade to the finale or seek to the last cue (cited) | `track_finish` + `cue()` sorted, `fMusicFinaleCrossFadeTimeIn/Out` | [`music.cpp:274-318`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L274-L318) | impl | Tolerance `k_tolerance_ms` | Past the last cue -> natural end |
| audio-runtime-music-ducking | Music ducking | A ducking type lowers the current track instead of replacing it | `type_duck`/`track_duck` with `fMusicDuckingSeconds`/`UnDuckingSeconds` | [`music.cpp:351-360`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L351-L360), [`:624-668`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L624-L668) | impl | Only single tracks duck | Palette/silent -> no-op |
| audio-runtime-music-cell | Cell music selection | XCMO, location music with parents, region music, worldspace music (cited) | Same order in `cell_music`, pushed on cell change | [`ambience.cpp:328-372`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L328-L372) | impl | Region music only for exteriors | No type -> NoMusic |
| audio-runtime-acoustic-space | Acoustic space activation | Entering a space fades its ambient loop in and sets its reverb; leaving restores the default (cited) | XPRM box test in the reference frame, 5-frame leave debounce, cell default space when nothing is active | [`ambience.cpp:224-260`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L224-L260), [`:439-482`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L439-L482) | impl | Sphere is treated as a cube (bounds equal) | No active space -> empty |
| audio-runtime-reverb-transition | Reverb smoothing | After every XAudio2 pass each I3DL2 field moves linearly to the target over `fReverbTransitionTime` (cited) | `on_processing_pass_end` on the XAudio2 callback, mB fields truncated | [`device.cpp:162-202`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L162-L202) | impl | Time from `GetTickCount`, not the audio clock | Target equal to done -> snap |
| audio-runtime-region-sounds | Region ambience | Override/priority region list, chance per pass, placement around the listener, budgets (cited) | 500 ms pass, `uRegionSoundsAllowedPerUpdate` budget, `chance*0.5` test, 4 draws for placement, loop entries when chance >= 1 | [`ambience.cpp:484-577`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L484-L577) | impl | A third draw is discarded to match the game's sequence (cited) | No region -> list cleared, loops fade |
| audio-runtime-weather-sounds | Weather sounds | Sky-only cells, transition volumes from `DATA` thresholds, thunder by frequency | Entries per (weather, descriptor, type), volume formulas per type, thunder retry every 30 ms | [`ambience.cpp:579-692`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L579-L692) | impl | Frequency 0 is special-cased to never thunder | Not sky -> all stopped at once |
| audio-runtime-water-ambience | Water ambience | Sampled wet points around the listener set one instance's volume and position (cited) | Volume = samples / (2 * axis points), fade in/out, position follows the smoothed centroid | [`ambience.cpp:693-725`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L693-L725), [`app_host.cpp:836-880`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_host.cpp#L836-L880) | impl | Sampling itself is engine-side | No water or 0 samples -> fade out |
| audio-runtime-underwater | Underwater | Mute-when-submerged categories, underwater loop, underwater reverb | Category mutes over 0.1 s, UWLS loop with flag 0x10, URVT reverb on enter | [`ambience.cpp:727-752`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L727-L752) | impl | none seen | No DOBJ UWLS -> no loop |
| audio-runtime-emitters | Placed emitters | Start when the cell's 3D loads, follow enable state, fade on unload (cited) | Loaded-cell grid `uGridsToLoad`, start at build, 1 s fade and erase when no longer wanted | [`ambience.cpp:396-418`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L396-L418), [`:754-795`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L754-L795) | impl | Conditions are evaluated once, at start (cited) | Failure -> id 0 kept, no retry |
| audio-runtime-footstep | Footsteps | First matching tag in the gait list decides; player uses Sound 2 with a player output model, NPCs Sound 1 pooled | Tag compare case-insensitively, `find_impact`, DOBJ POPM/P3OM override for the player, 8-slot pool | [`ambience.cpp:856-908`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L856-L908), [`:813-854`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L813-L854) | impl | Gait must be supplied by the caller | No match -> nothing |
| audio-runtime-impact | Impact effects | Both IPCT sounds at the point | Sound 1 and Sound 2, unpooled | [`ambience.cpp:909-925`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L909-L925) | impl | none seen | Missing pair -> nothing |
| audio-runtime-combat | Weapon impact | Sound 1, or Sound 2 for the player's normal hit | Same condition on `player_attacker && normal_hit` | [`ambience.cpp:926-941`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L926-L941) | impl | none seen | Missing pair -> nothing |
| audio-runtime-collision | Physics contact | `fMinSoundVel` gate, same-pair window, both directions, heavy threshold picks Sound 2, 6-slot pool (cited) | Same, with time-window dedupe and per-direction lookup | [`ambience.cpp:942-995`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L942-L995), [`:813-854`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L813-L854) | impl | Pool sizes are fixed (8 and 6) | Under threshold -> nothing |
| audio-runtime-havok-material | Havok material mapping | MATT whose lower-cased `MNAM` matches the Havok material name | Linear scan over materials comparing the name | [`system.cpp:1383-1400`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L1383-L1400) | impl | Linear search per call | Unknown -> 0 |
| audio-runtime-hrtf | Headphone HRTF | Vanilla "HRTF" is X3DAudio speaker panning with no height (cited) | Per-sound mono submix with a 64-tap XAPO; direction from the head frame every pass; level from a mono X3DAudio layout | [`binaural.cpp:64-90`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L64-L90), [`sound.cpp:335-350`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L335-L350), [`:407-427`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L407-L427) | impl | Deliberate upgrade; off by default | Off -> no voice exists |
| audio-runtime-hrtf-data | HRTF filter set | No equivalent | 306 directions on 13 elevation rings, 64 taps, ITD stored apart, resampled once per device rate | [`hrtf_data.inc:12-33`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/hrtf_data.inc#L12-L33), [`hrtf.cpp:56-80`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/hrtf.cpp#L56-L80) | impl | Generic KU100 head; front/back confusion possible | Device rate != 48 kHz -> resample once |
| audio-runtime-capture | Mix capture | No player-facing equivalent | XAPO on the mastering voice, lock-free ring, writer thread, 32-bit float WAV, mute by zeroing the output | [`capture.cpp:35-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L35-L68), [`:106-160`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L106-L160), [`device.cpp:84-90`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L84-L90) | impl | Tooling, not vanilla behaviour | Already running -> `start` false |
| audio-runtime-state-json | State snapshot | No equivalent | JSON published every 4th tick under a try_lock mutex | [`system.cpp:876-911`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L876-L911), [`:1469-1487`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L1469-L1487) | impl | Format is spud-specific | No device -> `{"available": false}` |
| audio-runtime-automation | Script commands | No equivalent | `audio ...` verbs mapped onto the API in the host (`play`, `speak`, `env`, `footstep`, `capture`, ...) | [`docs/AUDIO.md:137-161`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/AUDIO.md#L137-L161), [`app_host.cpp:890-1110`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/engine/app_host.cpp#L890-L1110) | impl | Harness, not gameplay | Audio unavailable -> error |
| audio-runtime-settings | Menu volumes and persistence | Master plus every "Should Appear on Menu" category (cited) | Slider list from category flags, keys `effects/footsteps/voice/music` or the lower-cased EDID, values written by the engine | [`system.cpp:991-1020`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L991-L1020), [`docs/AUDIO.md:58-77`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/AUDIO.md#L58-L77) | impl | The audio system reads no file itself | Missing key -> UNAM default |

## Support matrix

Measured with `rg`, `awk`, `comm` in the spud checkout.

| Dimension | Supported | Unsupported / defaulted | Command |
|---|---|---|---|
| Record signatures compiled into `sound.osd` | 26 (SNCT SOPM SNDR SOUN REVB ASPC MUSC MUST FSTP FSTS IPCT IPDS MATT REGN WTHR WRLD LCTN CELL REFR ACTI MSTT LIGH DOOR WATR ECZN DOBJ) | No `SCPT`, `QUST`, `PACK`, `IDLE`, `KYWD` audio links; `TACT` is declared but never matched (spud claims the game does the same) | `rg -c '^constexpr RecordType k_'` -> 69 constants (records plus subrecords) |
| `.osd` chunks | 34 chunks over 28 structs in `osd.hpp` | No version field beyond `k_version = 1`; no per-chunk presence flags | `rg -c '^struct ' src/formats/osd.hpp` -> 28; chunk list [`osd.md:14-49`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/formats/osd.md#L14-L49) |
| INI settings compiled | 42 | 36 read (34 by literal name plus the two audio sleep times read through a variable); unused: `fDefaultMasterVolume`, `fHardLandingDamageThreshold`, `uMainMenuMusicAttnmB`, `uMainMenuMusicFadeTimeMS`, `uWaterAudioAmbientAttenuation`, `uWaterAudioSamplePointsToUse` | `comm -23` of the two lists (see shell notes) |
| Condition functions with a real value | 6 (`GetCurrentTime` 18, `IsRaining` 62, `IsSnowing` 75, `GetIsCurrentWeather` 149, `IsInInterior` 300, `GetCombatTargetHasKeyword` 707 -> always 0) | Every other function returns 0; comparison against a non-zero value therefore fails | [`conditions.cpp:15-32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/conditions.cpp#L15-L32) |
| Condition form parameters resolved | 8 functions (69, 71, 72, 74, 359, 448, 560, 707) | Others keep raw parameter values | [`audio_import.cpp:166-175`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L166-L175) |
| DOBJ signatures read at runtime | 7 (DDSC DFTS NDSC PDLC PDMC PIMC SFDC) | `RVBT`, `UWLS`, `URVT`, `POPM`, `P3OM`, `BTMS`, `MDSC` are used through helpers, not directly enumerated | `rg -o 'default_object\("[A-Z0-9]{4}"\)'` |
| Audio file formats accepted | `.wav` (16-bit PCM), `.xwm`, `.fuz`, `.lip` | No `.mp3`, `.xma`, `.adpcm`; a WAV with any other tag loads at import but fails in `parse_wave` | [`audio_import.cpp:1184-1188`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1184-L1188), [`clip.cpp:73-78`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L73-L78) |
| Runtime decoders | RIFF PCM16, Ogg Vorbis (stb_vorbis) | No xWMA/FUZ at runtime; no resampler (device rate equals clip rate by XAudio2 conversion) | [`clip.cpp:161-192`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L161-L192) |
| Spatial modes | Uses HRTF (X3DAudio matrix), defined speaker output (5-point curve), no model (default matrix) | No cone/heading-oriented emitter; emitter `OrientFront` is fixed `(0,0,1)` | [`sound.cpp:376-386`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L376-L386) |
| Emitter kinds | sound_marker, acoustic_space, looping_object, light, door_loop | No projectile/explosion/region emitters at import | [`osd.hpp:363-370`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/formats/osd.hpp#L363-L370) |
| Voice pools | 8 footsteps, 6 collisions | No global voice cap or virtualisation found | [`ambience.cpp:131-140`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L131-L140) |
| Public API functions | 42 in `system.hpp` | No per-sound reverb send override, no submix graph, no listener velocity-driven Doppler control (velocity is passed through) | `awk 'NR>=51 && NR<=172' src/audio/system.hpp` |

## Codec and licence table

| Stage | Codec / component | Where | Licence status | Notes |
|---|---|---|---|---|
| Skyrim source audio | xWMA (WMA v2 tag 0x0161, v9 tag 0x0162) in a RIFF `XWMA` container | [`audio_codec.cpp:47-89`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L47-L89) | Game asset; not redistributable | `dpds` supplies the true length |
| Skyrim source audio | 16-bit PCM WAV, `smpl` loop chunk | [`clip.cpp:30-86`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L30-L86) | Game asset | Copied byte for byte |
| Skyrim voice | `.fuz` = `FUZE` + lip block + audio | [`audio_codec.cpp:248-259`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L248-L259) | Game asset | Audio part is xWMA or WAV |
| Skyrim lip | `.lip` / FUZ lip block, version 1 | [`audio_codec.cpp:261-306`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L261-L306) | Game asset | Becomes `.osv` |
| Decode | Media Foundation `CLSID_CWMADecMediaObject` (WMA decoder) | [`audio_codec.cpp:153-244`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L153-L244) | Proprietary OS component, Windows only; not redistributable, no source | `#ifdef _WIN32`; other platforms throw ([`:326`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L326)) |
| Encode | libvorbis VBR (`vorbis_encode_init_vbr`) | [`audio_codec.cpp:342-403`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L342-L403) | BSD-3-Clause upstream (not recorded in the repo; unverified here) | From vcpkg ([`vcpkg.json:7`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/vcpkg.json#L7)) |
| Container | libogg with fixed serial `0x4F534B59` | [`audio_codec.cpp:338`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_codec.cpp#L338) | BSD-3-Clause upstream (unverified here) | Makes re-imports byte-identical |
| Runtime decode | stb_vorbis (header-only) | `src/audio/stb_vorbis_impl.cpp`, [`clip.cpp:17`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.cpp#L17) | Public domain / MIT dual (upstream; unverified here) | Whole clip or stream |
| Mixing | XAudio2 2.9, X3DAudio, XAPO (`CXAPOBase`) | `device.cpp`, `binaural.cpp`, `capture.cpp` | Proprietary OS component, Windows only | Link set only when `WIN32` ([`src/audio/CMakeLists.txt:4-6`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/CMakeLists.txt#L4-L6)) |
| Room acoustics | XAudio2 `AudioReverb` + `ReverbConvertI3DL2ToNative` | [`device.cpp:139-171`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L139-L171) | Proprietary OS component | REVB maps 1:1 onto the I3DL2 fields |
| HRTF filters | SADIE II, subject D1 (Neumann KU100), 306 directions, 64 taps | [`src/audio/hrtf_data.inc:1-33`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/hrtf_data.inc#L1-L33) | **Apache-2.0**, University of York; notice kept at `tools/hrtf/LICENSE-SADIE-II.txt` | Derived work: reduced grid, diffuse-field equalised, minimum phase, ITD split out. This is the only third-party licence text in the tree |
| HRTF generator | `tools/hrtf/build_hrtf.py` (9,201 measurements -> 306) | [`docs/AUDIO.md:108-114`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/docs/AUDIO.md#L108-L114) | spud MIT; consumes Apache-2.0 data | Output is committed, not generated at build |
| Maths | glm | [`vcpkg.json:3`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/vcpkg.json#L3) | MIT | Vec/quat only |

Licence note for Mudcrab: the SADIE II data is Apache-2.0 and compatible with GPL-3.0-or-later distribution, but
it is *data*, so a redistributed pack must carry the notice. The Windows decode and mix path (Media Foundation,
XAudio2, XAPO) cannot be reused on macOS or Linux and cannot be shipped.

## Portable backend boundary

Spud has no backend abstraction. The dependency is spread over four translation units; [`sound.hpp:18`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.hpp#L18) forward
declares `IXAudio2SourceVoice` and `sound.cpp` mentions an XAudio2/X3DAudio/XAPO type or constant on 43 lines
(measured with `rg -c`). The boundary below is the minimum a replacement must provide, derived from every
XAudio2/X3DAudio call site.

### What the runtime actually needs

| # | Capability | Exact spud use | Call sites |
|---|---|---|---|
| 1 | Open a stereo/5.1 output at a requested rate; report the actual rate | 6-channel mastering voice, rate clamped 20000..48000 | [`device.cpp:42-58`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L42-L58) |
| 2 | Master bus + separate reverb bus with a configurable room effect | `CreateSubmixVoice(2 ch)` + `AudioReverb` | [`device.cpp:67-80`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L67-L80) |
| 3 | Set reverb from 13 I3DL2 parameters | `ReverbI3DL2` -> native conversion | [`device.cpp:139-161`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L139-L161) |
| 4 | "Processing pass ended" callback | Drives the linear reverb transition | [`device.cpp:162-202`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L162-L202) |
| 5 | Create a source voice with N channels at a rate, two filtered sends, a frequency-ratio range, and buffer callbacks | One per sound | [`device.cpp:115-136`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L115-L136) |
| 6 | Submit a PCM buffer with loop begin/length/count and an end-of-stream flag | Whole clips, loops | [`sound.cpp:138-161`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L138-L161) |
| 7 | Start, stop, pause, flush buffers, exit loop, destroy, set volume, set frequency ratio | All playback control | [`sound.cpp:124-135`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L124-L135), [`:474-565`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L474-L565) |
| 8 | Set a per-destination output matrix from S source channels to D destination channels | Master, reverb, binaural submix | [`sound.cpp:255-281`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L255-L281), [`:398-427`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L398-L427) |
| 9 | Set a per-destination low-pass coefficient | Distance muffling | [`sound.cpp:428`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L428) |
| 10 | Report frames played and buffers queued | Position, seek base, end detection | [`sound.cpp:163-175`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L163-L175), [`:289`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L289), [`:508`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L508) |
| 11 | Seek by flushing and resubmitting from a sample offset | Music loop and cue seeks | [`sound.cpp:178-198`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L178-L198), [`system.cpp:194-204`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L194-L204) |
| 12 | Spatial maths: listener + emitter -> matrix, direct LPF, reverb LPF, reverb level, Doppler, given a 6-point distance curve, min/max distance, inner radius, source/dest channel counts | `X3DAudioCalculate` with 5 flags; a second mono-layout call for HRTF level | [`sound.cpp:351-431`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L351-L431) |
| 13 | A mono submix voice with a custom effect, matrixed to FL/FR | HRTF voice | [`binaural.cpp:64-90`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L64-L90) |
| 14 | An effect processor with lock/process and a silent-buffer signal | HRTF renderer and capture tap | [`binaural.cpp:46-57`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L46-L57), [`capture.cpp:40-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L40-L68) |
| 15 | An effect on the master that receives float frames before the device down-mix, plus a mute that happens after the tap | Capture | [`device.cpp:84-90`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L84-L90), [`capture.cpp:60-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L60-L68) |

### Proposed trait shape (Rust, for Mudcrab)

```rust
trait AudioBackend {
    fn open(&mut self, desc: DeviceDesc) -> Result<()>;   // rate, channels, silent
    fn sample_rate(&self) -> u32;
    fn create_bus(&mut self, ch: u16, kind: BusKind) -> BusId;   // Master | Reverb | Effect
    fn set_reverb(&mut self, bus: BusId, p: ReverbI3dl2);
    fn on_render_pass(&mut self, f: &mut dyn FnMut(&mut Self));
    fn create_voice(&mut self, d: VoiceDesc) -> Result<VoiceId>;
    fn submit(&mut self, v: VoiceId, b: Buffer<'_>);
    fn control(&mut self, v: VoiceId, c: Control);       // Start|Pause|Stop|Flush|ExitLoop|Destroy
    fn set_volume(&mut self, v: VoiceId, g: f32);
    fn set_rate(&mut self, v: VoiceId, ratio: f32);
    fn set_matrix(&mut self, v: VoiceId, to: BusId, src: u16, dst: u16, m: &[f32]);
    fn set_lowpass(&mut self, v: VoiceId, to: BusId, coefficient: f32);
    fn state(&self, v: VoiceId) -> VoiceState;            // frames_played, buffers_queued
    fn seek(&mut self, v: VoiceId, frame: u64);
}

trait Spatialiser {                                        // X3DAudio replacement
    fn calculate(&self, l: &Listener, e: &Emitter, curve: &DistanceCurve,
                 src: u16, dst: u16) -> SpatialResult;      // matrix, lpf_direct, lpf_reverb, reverb, doppler
    fn levels_only(&self, l: &Listener, e: &Emitter, curve: &DistanceCurve, src: u16) -> Vec<f32>;
}

trait RenderEffect {                                       // XAPO replacement
    fn lock(&mut self, max_frames: u32);
    fn process(&mut self, input: Option<&[f32]>, output: &mut [f32], frames: u32) -> bool; // false = silent
}
```

### Mapping spud call -> trait -> Seedling/Firewheel capability

Seedling/Firewheel were not read in this pass; the right-hand column is `unverified`.

| spud call | Trait method | Seedling/Firewheel (unverified) |
|---|---|---|
| `Device::create_source` | `create_voice` | Node with a sample player; per-node volume/pan |
| `m_voice->SetOutputMatrix` | `set_matrix` | Channel routing exists; arbitrary S->D matrices at runtime are the open question |
| `X3DAudioCalculate` (matrix, LPF, reverb, Doppler) | `Spatialiser::calculate` | Firewheel has spatial nodes; distance curves and per-destination LPF are not X3DAudio-compatible by default |
| `X3DAudioCalculate` on the mono layout | `Spatialiser::levels_only` | Needs a one-channel layout; may need reimplementation |
| `SetOutputFilterParameters` | `set_lowpass` | Biquad/custom processor per node |
| `SubmitSourceBuffer` with loop fields | `submit` + `Buffer::loop_` | Looping exists; `smpl`-chunk loop points may need mapping |
| `GetState().SamplesPlayed` | `state().frames_played` | Position reporting must be exact: music cues, loop ends and subtitle timing depend on it |
| seek by flush+resubmit | `seek` | May require re-creating the sample player |
| `CreateSubmixVoice` + `AudioReverb` + I3DL2 conversion | `create_bus` + `set_reverb` | Firewheel has convolution/algorithmic reverbs but not I3DL2 parameter parity; a 13-field adapter is needed |
| `RegisterForCallbacks` + `OnProcessingPassEnd` | `on_render_pass` | Firewheel processes blocks; expose a block boundary instead |
| `BinauralVoice` + `BinauralXapo` | `create_bus(Effect)` + `RenderEffect` | Effects/processors exist; a 64-tap stereo convolver is a small local implementation |
| `CaptureTap` XAPO | master-effect tap | Needs a tap point before the device, independent of output mute |

What has no counterpart anywhere, and must be written: the X3DAudio distance-curve/inner-radius maths, the I3DL2
parameter set and transition, and the exact 0.01 dB volume arithmetic (that part is backend-independent and
already lives in [`sound.cpp:458-472`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L458-L472)).

### Event surface a producer must supply

This is the complete set of inputs the runtime takes; anything not listed is decided inside it. Units are world
metres (Z up) unless stated, and every call is asynchronous.

| Event | Arguments | Producer | Notes |
|---|---|---|---|
| Play descriptor | descriptor index, optional position, optional output-model override | anything | Conditions are evaluated inside, once |
| Play file | cache path, category, output model, optional position | dialogue | Lip data is a separate channel |
| Speak | cache path, optional position, output model, in-dialogue flag | dialogue/actors | Chooses the highlight sub-category by distance |
| Stop / set volume / set position | sound id | any owner | Ids are generational in the API but plain u32 here |
| Listener | position, forward, up, velocity | camera | Per frame |
| Environment | world, interior cell, game hour, weather + previous weather + transition fraction, underwater, water FormID, wet-sample count, wet centroid | world/weather/water | Per frame |
| Music add / remove / stop all | music-type index | combat, scripts, menus, quests | Heap rules apply inside |
| Footstep | footstep set, gait, tag string, material FormID, position, is-player, first-person | animation | Gait and tag come from the annotation |
| Impact | impact set, material, position | projectiles, explosions | Both IPCT sounds |
| Combat impact | impact set, material, position, player attacker, normal hit | combat | Sound 1 or 2 |
| Collision | material A, material B, position, speed m/s | physics | Threshold and dedupe inside |
| Havok material map | Havok material id | physics | Returns a MATT FormID |
| Animation event | reference, annotation string, reference position, optional node position | animation | `SoundPlay.*`, `SoundStop.*`, `SoundRelease.*`, `SoundAnimationEnd` |
| Animation move | reference, position | animation | Moves all sounds of that reference |
| Menu / loading / dialogue open | bool | UI | Category pause and duck |
| Settings | master, per-category, HRTF on/off | UI | Values persist outside the audio system |
| Capture / state | file path and seconds, or nothing | tools | Not gameplay |

## Mudcrab mapping

| Ledger group | Mudcrab state | Gap | Where it would live | Issue |
|---|---|---|---|---|
| A. Record ingestion | `crates/converter/src/records/families/audio_extras.json` (1385 lines) already declares SOUN, SNDR, SOPM, SNCT, MUSC, MUST, ASPC, REVB, FSTP, FSTS, DOBJ; MATT/IPCT/IPDS/WTHR in `visual_extras.json`; WATR and CELL `XCAS/XCMO/XCLR/XCWT/XCLW` in `schema.json`; LCTN in `dialogue.json` | REGN has `WNAM`, `RPLI/RPLD` but no `RDMO`/`RDSA`/`RDAT`; no projection from decoded records to a runtime audio view; no sound-file bytes in any pack | `crates/converter/src/records/families/world_extras.json` (REGN), new audio projection module under `crates/converter/src/` | #306 (projection), #303 (inventory) |
| A. Emitters and cells | CELL audio fields decoded; no audio emitter extraction, no line-endpoint maths, no enable-chain evaluation for audio | The whole `Emitter` table has no equivalent | Across converter world and audio projection | #306, #309 |
| B. Files and codecs | Archive selection excludes audio (`archive/mod.rs:929-946`: `dds nif pex lod strings ilstrings dlstrings`); `crates/converter/src/lip.rs` decodes LIP to visemes (327 lines, 16 visemes, 33 slots) | No audio `AssetKind`, no xWMA decode, no Vorbis encode, no `.fuz` demux, no audio recipe version | `crates/converter/src/asset_path.rs`, `crates/converter/src/pipeline.rs`, new codec module | #521 (recipe), #522 (loose overlay), #305, #304 (codec route) |
| B. Codec licence route | #304 already rejects Symphonia-only (no WMA) and names FFmpeg xwma/wma as the candidate; #305 asks float32-first comparison and FLAC | spud's route (Media Foundation + libvorbis) is not portable and not FOSS-clean for the decoder | Codec module in the converter | #304, #305 |
| C. Playback core | No audio crate; `bevy` is built with the `audio` feature (`Cargo.toml:23`) but nothing uses it | Voice lifecycle, command ring, loader, clip cache, volume arithmetic | New `crates/engine` audio module, or a new `crates/audio` | #302, #308 |
| C. Categories and fades | Absent | SNCT chain, pause/mute/attenuation broadcast, 0.01 dB arithmetic | Engine audio module | #308 |
| C. Music | Absent | Heap, types, palettes, cues, finale cross-fade, ducking, cell music | Engine audio module + world integration | #309 |
| C. Ambience | Absent | ASPC/REVB, region sounds, weather sounds, water, emitters, underwater | Engine audio module + world | #309 |
| C. Gameplay producers | Absent; no physics material callback, no footstep annotation consumer | FSTP/FSTS/IPDS/MATT chain, animation sound annotations | Engine audio module + producers | #310 |
| C. Voice and lip | `crates/converter/src/lip.rs` decodes LIP; no playback, no `.fuz`/FUZ audio path | Voice lookup by voice type, subtitle pairing, playback clock | Engine dialogue + audio | #311 |
| C. HRTF | Absent; #303 freezes stereo/headphone output and says a mandatory HRTF renderer is not implicit | Optional, off by default; if wanted, SADIE II is the candidate data (Apache-2.0) | Engine audio module, optional feature | #303, #304, #313 |
| C. Diagnostics | Absent | State snapshot, offline capture, deterministic seed | Test/tooling | #313 |
| D. Native boundary | Pure Rust so far; only `windows-sys` for COM/shell | Any C/C++ audio path needs the A05 ownership contract | n/a | #307 |
| Packaging | GPL-3.0-or-later workspace | No audio dependency yet; Seedling/Firewheel are MIT OR Apache-2.0 | n/a | #312 |

Design fit notes: Mudcrab is Bevy 0.19 + Luau; #302 fixes Seedling 0.8 + Firewheel 0.12.1 as the target, so the
trait above should be implemented *over* Firewheel nodes rather than beside them. The converter already has an
immutable registered-pack model, so the `.osd` idea maps to a new pack asset kind rather than a side file. Music
and dialogue timing need a sample-accurate clock in the ECS, which is a new requirement on the engine's clock
work.

## Issue routing (#302-#313, #521, #522)

| Issue | Findings from this report that feed it |
|---|---|
| #302 charter | The dependency table and the trait surface show the whole runtime is XAudio2/XAPO/Media Foundation, so the FOSS-core boundary is not a formality: rows `audio-runtime-device`, `audio-codec-xwma-decode`, `audio-runtime-hrtf`. Also the licence table: only the SADIE II notice is retained in-tree. |
| #303 scope, inventory, budgets | The 26-signature set and its chunk split (`audio-import-record-set`); unit/rate facts (`audio-runtime-rate-clamp`); the HRTF-off-by-default policy (`audio-runtime-hrtf`); the open voice-limit question; 306-direction data as the only candidate HRTF corpus. |
| #304 target and codec route | Codec and licence table; spud decodes with a proprietary Windows decoder and re-encodes Vorbis (`audio-codec-vorbis-encode`, `audio-codec-vorbis-quality`), which is exactly the extra perceptual generation #304 warns about; stb_vorbis/libvorbis obligations are unrecorded (`audio-runtime-stream-threshold` uses stb at runtime). |
| #305 ingestion | Record-versus-file split (`audio-import-record-set`, `audio-codec-cache-path`); extension fallback (`audio-import-ext-fallback`); determinism (`audio-codec-determinism`); incremental skip without a version marker (`audio-codec-skip`); float32-first comparison is the right gate (own added idea). |
| #306 projection and producer contracts | The `.osd` chunk layout as a candidate runtime audio view (`audio-import-index-identity`, `audio-import-string-table`); per-family semantics for SOPM/SNCT units and curves (`audio-import-sopm-*`); REGN chances kept distinct (`audio-import-regn`); conditions as data (`audio-import-ctda`); emitters and line endpoints (`audio-import-emitter-*`). |
| #307 native boundary | The trait surface and its ownership notes: one owner per voice/bus, control by generational id, no allocation in `process`, retire callbacks before freeing (`audio-runtime-command-ring`, `audio-runtime-loader`, `audio-runtime-hrtf`, `audio-runtime-capture`). |
| #308 playback, selection, mixing | `audio-runtime-file-choice`, `-frequency`, `-static-attenuation`, `-volume`, `-category-chain`, `-category-state`, `-fade`, `-loop-envelope`, `-distance-cull`, `-3d`, `-speaker-output`, `-default-matrix`, `-sound-cache`, `-stream-threshold`, `-source-voice`, `-device`. |
| #309 ambience, acoustics, music | `audio-runtime-acoustic-space`, `-reverb-transition`, `-region-sounds`, `-weather-sounds`, `-water-ambience`, `-underwater`, `-emitters`, `-music-heap`, `-music-track`, `-music-palette`, `-music-cues`, `-music-ducking`, `-music-cell`. |
| #310 gameplay, UI, material producers | `audio-runtime-footstep`, `-impact`, `-combat`, `-collision`, `-havok-material`, `-animation-sounds`, `-menu-pause`, `audio-import-fsts-order`, `audio-import-ipds`, `audio-import-matt`. |
| #311 voice and shared timing | `audio-runtime-speech-highlight`, `-dialogue-duck`, `-default-matrix` (`play_file` with DOBJ 147), `audio-codec-lip-decode`, and the sample-accurate position requirement in `-music-cues` / port note 6. |
| #312 packaging | Codec and licence table: which artifacts may ship (libvorbis, stb_vorbis, SADIE II data) and which may not (Media Foundation, XAudio2, XAPO). |
| #313 acceptance | `audio-runtime-state-json`, `-capture`, `-automation`, `-settings`, plus the unmeasured claims (33 ms cadence, 0.12 dB resampling parity, XAudio2 2.9 vs 2.7) and the offline-render parity test idea. |
| #521 archive recipe | Measured: `selection_includes` accepts only `dds nif pex lod strings ilstrings dlstrings` plus animation inputs (`crates/converter/src/archive/mod.rs:929-959`); spud's audio files sit under `sound/` and `music/` with `.wav/.xwm/.fuz/.lip` ([`audio_import.cpp:1184-1188`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1184-L1188)). |
| #522 loose overlay | Spud resolves loose files through the same data source as archives and applies the same extension fallback ([`audio_import.cpp:256-275`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L256-L275)), so loose-over-archive order is a single lookup: evidence that one canonical path plus collision error is enough. |

## Port notes

1. **33 ms pass, 66 ms poll** ([`system.cpp:805-938`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L805-L938), [`sound.cpp:501`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L501)). Music cues, fades and speech ducking all
   quantise to 33 ms. A Bevy system at frame rate will produce different transitions unless it keeps an audio
   clock separate from the render clock.
2. **Menu mode sleeps 1 ms** (`uAudioThreadSleepTimeMenuMode`, [`system.cpp:930-933`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/system.cpp#L930-L933)). That is a busy loop by
   design; do not copy it into an ECS schedule.
3. **Coordinate swizzle**: positions are Z-up metres; X3DAudio is Y-up left-handed, so `to_x3d` passes
   `(x, z, y)` ([`sound.cpp:33-36`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L33-L36)). Any backend with a different convention needs one conversion point.
4. **Units**: 1 Skyrim unit = 0.0142875 m; inner radius 69.99 units = 1.0 m; speed of sound 24041.564453125
   units/s (343.5 m/s) ([`device.hpp:96`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.hpp#L96), [`sound.cpp:25`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/sound.cpp#L25)). Region placement, water sampling and radar-style range
   tests all mix units and metres; every one of them is a place to get the scale wrong.
5. **Conditions gate sounds silently**. Unknown condition functions return 0 ([`conditions.cpp:15-32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/conditions.cpp#L15-L32)), so an SNDR
   with `GetIsRace == Nord` never plays. Mudcrab already has a shared condition evaluator; route audio conditions
   through it instead of copying this switch, or whole categories of sounds will be missing with no error.
6. **Music depends on seek and position**. Loop counts, cue points and the finale cross-fade read
   `position_ms` every pass ([`music.cpp:201-318`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/music.cpp#L201-L318)). A backend without sample-accurate position and seek cannot
   reproduce music.
7. **16-bit PCM everywhere**. Clips, buffers and the volume arithmetic are i16/0.01 dB; only HRTF and capture use
   float ([`clip.hpp:27-30`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/clip.hpp#L27-L30), [`binaural.cpp:53-56`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/binaural.cpp#L53-L56)). Convert once at the boundary, not per block.
8. **Reverb transition needs a render-pass hook** ([`device.cpp:162-202`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L162-L202)). Firewheel processes blocks; the
   transition must be driven from the block callback, not the game loop.
9. **Import ordering constraints**: categories and output models before descriptors, descriptors before markers,
   tracks before types, materials and impact sets before pairs, regions and weather before cells
   ([`audio_import.cpp:208-228`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L208-L228)). Any rewrite must keep topological order or resolve in a second pass.
10. **Gait list reversal** ([`audio_import.cpp:663-680`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L663-L680)) and **region `RDAT` type-7 gating**
    ([`:704-724`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L704-L724)) are easy to get wrong from the record layout alone.
11. **Weather volume thresholds come from `WTHR.DATA` bytes 6-11** ([`ambience.cpp:626-672`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L626-L672)). The meaning of those
    bytes is asserted, not documented (`unverified` against the record spec).
12. **Mute must not silence the capture** ([`device.cpp:204-206`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.cpp#L204-L206), [`capture.cpp:60-68`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/capture.cpp#L60-L68)): mute is applied inside the
    tap, after recording.

Minimal vertical slice for Mudcrab: one interior cell with an `ASPC` (ambient loop + `REVB`) and one exterior
cell with a `SOUN` marker whose `SNDR` has two `.wav` files and an `SOPM` curve; a headless scene that plays the
descriptor at 3 distances and once on entering the space. Checks: the file choice alternates and never repeats,
the measured gain follows the 5-point curve within 0.1 dB, the ambient loop fades in over the `ASPC` fade time,
`REVB` reaches the reverb bus as 13 I3DL2 fields, and `state` reports descriptor, distance and attenuation.
Second check: play a 3-cue music track and assert the finale cross-fade starts within one pass of cue 2.

## Test ideas

1. Converter projection golden test: synthetic SNDR/SOPM/SNCT/MUST records -> one runtime audio asset; assert
   index links, units (metres), flag remaps (0x10->0x800 etc.), and that a missing file yields `none` plus a
   diagnostic, not a panic.
2. Extension fallback table test: same base name present as `.wav` only, `.fuz` only, `.xwm` only, none; assert
   the order `.wav` > `.fuz` > `.xwm` and the cache extension mapping.
3. Codec round-trip: one xWMA fixture decoded with the chosen route, compared to an FFmpeg reference on float32
   PCM (max abs error, length, sample rate, channels); then the Vorbis encode compared on decoded length and a
   spectral distance bound.
4. `vorbis_quality_for` table test (channels, source bytes/s) -> quality; catches the halved-table heuristic.
5. Condition parity: table of the implemented functions (5 with real values per [`conditions.cpp:15-32`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/conditions.cpp#L15-L32); the original note said 6, counting the 707 stub) x 6 operators x OR-group shapes, compared to the shared
   Mudcrab condition evaluator; unknown function must be `false` and must be logged once.
6. Category arithmetic: parent chain with pause at the root -> 4000 mB; attenuation sum capped at 10000; volume and
   frequency multiply up the chain; cycle terminates.
7. Music heap: insert with equal priorities, assert stable order and that `remove` of a non-top entry sifts down
   only.
8. Music cue test: 3 cue points, assert the seek/finale target in each pass with a 33 ms tolerance.
9. Region placement determinism: fixed seed -> assert the four draws and the discarded third, and that the
   placement lands in the documented annulus.
10. Weather volume table: `pct` sweep against `DATA` thresholds for normal/precipitation/thunder, including the
    frequency-0 case (never thunders).
11. Footstep set: synthetic FSTS with distinct tags per gait, assert the reversed `DATA` order maps to walk..swim
    and that the first tag match wins.
12. Collision dedupe: same material pair twice inside `iCollisionSoundTimeDelta` -> one sound; reversed pair also
    suppressed; below `fMinSoundVel` -> nothing.
13. HRTF: direction -> four-tap interpolation weights sum to 1; ITD sign flips across the median plane; 44.1 kHz
    filters within 0.12 dB of 48 kHz (their claim, worth re-measuring).
14. Offline render parity: play a known descriptor through the backend's offline graph and compare gain per block
    to the 0.01 dB formula; this is the test that proves a replacement backend, and spud has no equivalent.

## Open questions

1. Is there a vanilla global voice limit or per-category virtualisation? Spud has none beyond the two fixed pools
   ([`ambience.cpp:131-140`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L131-L140)). Cheapest check: search the game INI/GMST for voice-count settings and observe a
   crowded scene.
2. Do spud's XAudio2 2.9 measurements hold for 2.7 (the version the game ships)? The doc claims measured parity
   ([`device.hpp:8-10`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.hpp#L8-L10)); the measurements were not reproduced here.
3. What are `WTHR.DATA` bytes 6-11 exactly? The code treats them as precipitation/thunder begin-end thresholds and
   a thunder frequency ([`ambience.cpp:626-672`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/ambience.cpp#L626-L672)). Cheapest check: xEdit field names for WTHR `DATA`.
4. Which condition functions actually occur on audio records in a real load order? The corpus scan would size the
   risk in port note 5. Cheapest check: count distinct CTDA function indices on SNDR/MUST records.
5. Does any shipped audio use `RDAT` types other than 7, or `SNDR`/`SOPM` `CNAM`? The importer ignores them
   ([`audio_import.cpp:704-724`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L704-L724), [`:335-351`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L335-L351)).
6. Is a FUZ audio part ever plain WAV? The code allows it ([`audio_import.cpp:1213-1216`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1213-L1216), [`:1240-1246`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/importer/convert/audio_import.cpp#L1240-L1246)); no
   sample was found.
7. What is the correct reverb parameter mapping from `REVB` to a non-XAudio2 reverb? I3DL2 is an IASIG
   specification; its field set is fixed ([`device.hpp:32-46`](https://github.com/chasmlol/spud/blob/8c9904207a5c573a0193fb903fb5c433f127f102/src/audio/device.hpp#L32-L46)), but Firewheel's reverbs take different parameters;
   the mapping is unattempted anywhere in this read.


