# Skyrim reference import, 2026-10-10

The three supplied archives provide useful composition and appearance references.
They do not establish a matched vanilla SE/AE frame. The supplied camera poses
include fits and estimates, and the sampled UESP sources have mixed upload dates. One imported
image matches a pre-SE source exactly; the edition and rendering state of the
other sampled imported images remain unverified.

The [JSON report](skyrim-reference-import-20261010.json) records archive/file pins,
the primary-source history sample, pose conflicts and fixture limits. This audit
does not change renderer defaults. It follows the evidence boundaries in the
[rendering map](skyrim-render-map/README.md) and
[remediation plan](../specs/engine/skyrim-rendering-remediation-plan.md).

## Import identity

The private import contains **91 unique files, 78 JPEG images, 84 poses and six
pose files**. All three archive hashes and all 91 imported file hashes/sizes were
reread and matched the immutable import receipt. The complete imported file
inventory also matched. These checks establish transferred bytes; they do not
establish original capture state or retail equivalence.

| Archive | SHA-256 |
| --- | --- |
| `chef-reference-shots-2026-10-10-part1of3.zip` | `da1ba92360119a4d2c18fdfe4a1c08ba8567f43606d04b4fa3eaeb8f9e2f415f` |
| `chef-reference-shots-2026-10-10-part2of3.zip` | `495115ca7d6abc7f52060b2a11b79eb8d6317d1ba006d66fbc66247f3050ae9f` |
| `chef-reference-shots-2026-10-10-part3of3.zip` | `5f21af3c37120df7a36735ab89c6497e57387bfc6c1ae8001e9e5062dedf8dc2` |

Archives remain private in `/Users/taylor/Downloads/`. The extracted files and
receipts are under
`/Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/imports/chef-reference-shots-2026-10-10/`.
`import-receipt.json` is SHA-256
`787ed6c00be1617e950c6ed7a61318df4625059a5e9487d93ccfb147cf3f374b`;
`pose-inventory.json` is
`79504e4219a9c06b106bc5975f02df24e893ce70835c348f4d54761e522a44b4`.
The original receipts retain their pending provenance/retail status. The new
report records the narrower checks completed here.

## Source histories and edition limits

UESP's primary MediaWiki API returned file histories for ten Riverwood images
and four representative world images. Ten of those named sources have current
uploads from 2012–2013; four have later uploads. Special Edition released in
October 2016, so the earlier uploads predate it. This is a chronological
inference, not a mod/settings audit.
[Bethesda's release notice](https://bethesda.net/en-US/news/skyrim-special-edition-now-available)
records 28 October 2016.

The imported `SR-place-Sleeping_Giant_Inn.jpg` **exactly matches** UESP's current
original SHA-1 `b8fdf39f349b2bdf2a32201e1f83260bc9c8037e`, uploaded
**2012-04-08 02:24:25 UTC**, at 1024×768. Its imported SHA-256 is
`4569cc9f32b5c9fe6e0bf02c82796fa415780fb9acd7512da557d8a1392f47ac`.
That pins this supplied image to a pre-SE source. It does not establish a
vanilla mod list, weather, time or display settings.
[UESP records that file identity and date.](https://en.uesp.net/wiki/File:SR-place-Sleeping_Giant_Inn.jpg#filehistory)

| Named UESP source | Current original upload | Imported-byte qualification |
| --- | --- | --- |
| [Riverwood Trader exterior](https://en.uesp.net/wiki/File:SR-place-Riverwood_Trader.jpg#filehistory) | 2012-02-04 | Imported bytes differ from the current original. |
| [Guardian Stones](https://en.uesp.net/wiki/File:SR-place-Guardian_Stones.jpg#filehistory) | 2012-10-28 | Imported bytes differ from the current original. |
| [Hjaalmarch](https://en.uesp.net/wiki/File:SR-place-Hjaalmarch.jpg#filehistory) | 2012-02-18 | Imported bytes differ from the current original. |
| [Riverwood aerial](https://en.uesp.net/wiki/File:SR-place-Riverwood.jpg#filehistory) | 2021-09-04 | Imported bytes differ; source page does not identify SE/AE. |
| [Riverwood street](https://en.uesp.net/wiki/File:SR-place-Riverwood_02.jpg#filehistory) | 2022-11-07 | Imported bytes differ; source page does not identify SE/AE. |
| [Solitude wide view](https://en.uesp.net/wiki/File:SR-place-Solitude_02.jpg#filehistory) | 2020-06-30 | Imported bytes differ; source page does not identify SE/AE. |

The JSON retains all 14 sampled histories and per-file comparisons. **Thirteen
sampled imported JPEGs do not match the current UESP originals.** Resizing and
re-encoding are documented in the supplied source notes, but this audit does not
prove which transformation or source revision produced each nonidentical file.
A filename and similar dimensions do not close that connection. Later upload
dates alone do not identify the edition. The remaining older named sources in
the sample include Trader, Alvor and Faendal interiors, Faendal's exterior,
Falkreath and Whiterun Hold; the later Inn common-room source is also
edition-unverified.

The raw primary API response is private as
`uesp-file-histories-sampled-20261010.json`, SHA-256
`1519a2d492c7a4068257d232adc93f411a862b9ba804607f4dc6cda51a804c67`.
The JSON report preserves its query URL and response identity. Browser-tool page
fetches returned 403; the read-only primary API query returned 200.

The Riverwood `SOURCES.txt` omits four present files: Alvor's interior, the Inn
common room, Faendal's exterior and `Riverwood_02`. It lists a Fishing image that
is absent from this import. The supplied README's claim of a source entry for
every image is therefore incomplete for Riverwood.

## Pose and aspect conflicts

`RW-02-street-to-the-inn` has numeric position
`[18742.6, -46745.8, 2.6]`, yaw `87.3`, pitch `-4.5` and horizontal FOV `75`.
Its own JSON note discusses different XY positions, `[18700, -46700]` and
`[17500, -47500]`. The older 2026-09-23 research document proposes
`[18700, -46700, -62]`, yaw `75.5`, pitch `-0.6`, FOV `75`. The JSON note dates its
later hand fit to 2026-09-24. These are distinct candidates; the new fixture
retains the imported numeric fields and leaves the conflict unresolved.

`RW-04-inn-front` is a hand fit graded *close* in its note. It retains
`[22073.9, -44520.5, 458.7]`, yaw `87.4`, pitch `11.7`, FOV `75`. The note says the
reference camera is nearer and looks down more steeply. The byte-matched Inn
image does not supply measured retail camera coordinates.

The older Riverwood research document says no render was made in that research
pass and describes an earlier truncated Faendal image. The currently imported
Faendal JPEG displays a complete house front. The later JSON contains hand-fit
poses and different framing notes. Keep those revisions distinct; neither the
README's word *matched* nor a `hand-fit`/`auto-refined` label establishes a retail
pose receipt. Full visual decoding here was limited to Faendal's image.

The imported inventory records three aspect mismatches. JPEG SOF headers were
read for all 78 images, and fixture/reference dimensions were checked across all
84 pose rows; 78 rows have reference images and six have none. These are header
checks, not full visual decoding.

| Shot | Imported fixture | Imported reference |
| --- | --- | --- |
| `RW-01-village-from-the-south` | 1400×1050 | 1400×788 |
| `RW-02-street-to-the-inn` | 1400×1050 | 1400×788 |
| `SR-photo-White_Run,_Red_Sky` | 1400×788 | 1400×875 |

Two additional small integer-dimension ratio differences are labeled matching
by the inventory: Blackreach 04 uses 1718×1080 against 1400×880, and Whiterun uses
1400×1050 against 1333×1000. The report preserves the three recorded mismatches
and lists those two rounding differences separately. The inventory's matching
threshold was not independently established.

For RW-02, preserving horizontal FOV `75` while changing the output from
1400×1050 to 1400×788 changes vertical FOV from **59.8404° to 46.7186°**.
This follows the perspective formula
`vfov = 2 * atan(tan(hfov / 2) * height / width)`. It corrects the output aspect
for the imported reference; it does not prove the source camera's FOV or pose.

The Hjaalmarch candidate is unsuitable as a fog validation view without further
work. `world_shots_4x3.json` pairs the exterior saltmarsh image with
`interior_cell_id: 50418004`, no worldspace, position `[599.2, -2829.6, 149.8]` and
an unchecked BYOH-house entrance note. That space/subject mismatch remains open.

## Repository fixtures and acceptance

Two small fixtures retain three imported candidate poses:

- [Inn and Faendal, 1400×1050](../../crates/engine/tests/fixtures/lighting-reference-riverwood-shots.json): unchanged numeric poses and angles; 4:3 output matches both imported reference aspects.
- [Riverwood street, 1400×788](../../crates/engine/tests/fixtures/lighting-reference-riverwood-wide-shots.json): unchanged numeric pose and angles; only output aspect changes from the imported 4:3 fixture.

Names, worldspace/interior fields, positions, yaw, pitch, horizontal FOV,
reference and confidence fields were compared with the immutable imported JSON
and matched for all three shots. The new fixtures and their hashes are recorded
in the report. This report does not record an engine run or accept their poses.
Reference images remain in the private import; the fixtures carry relative
reference paths for comparison tooling, not public image copies.

Use these candidates first to fit landmarks and compare subject, layout and
qualitative appearance. Review door, roof, porch and walkway alignment before
grading lighting. A SE/AE lighting comparison still needs edition/build,
plugin/resource winners, mod state, camera projection, weather/transition, hour,
IMGS/IMAD and adaptation history, effective settings and output/capture transfer.
Those values are unrecorded. Do not derive fog, exposure, gamma, saturation or
sun defaults from these images, or turn JPEG pixel differences into a native
parity result.
