# Earlier brightness checkpoint, 2026-10-10

The user rejected the latest lighting images as worsening. The earlier brighter
build has been recovered as a frozen test checkpoint. Its oversaturation and
clipped highlights remain unresolved. The user preferred its brightness; that
does not establish acceptance of the complete image.

The first normalization change removed the native photographic multiplier
before clamps and fog. The next change reduced exposed Bevy terrain/fallback
contributions. Later corrections changed native texture sampling and sun
direction. Combining selected pieces of those stages would create a different
pipeline from the earlier checkpoint.

| Stage | Output/input change |
| --- | --- |
| Earlier brighter build | Native response uses `(12000/pi) * Exposure(EV100=9.7)`, approximately 3.827, before clamps/fog; fallback exposure uses EV100 9.7 |
| Native normalization | Native response scale becomes 1 with camera exposure bypassed; fallback exposure stays unchanged |
| Shared-unit preview | Fallback EV100 becomes 11.636216, reducing exposed lit contributions by approximately 3.827 |
| Native texture views | Source-qualified diffuse sampling bypasses the compatibility sRGB decode; PBR views are preserved |
| Local sun trajectory | Replaces approximate noon zenith with recovered local arithmetic under an assumed identity parent |

SunlightScale remains 2.8, applied once. The saved frames retain sunlight RGB,
ambient rows, fog parameters and authored image-space controls. Pre-HDR changes
also affect clamps, adaptation and bloom, so a later brightness multiplier
cannot reproduce the earlier frame.

The recovery launcher is `scripts/skyrim-world-lighting-baseline.sh`. It pins
engine SHA-256 `7860c796a0fba9f240c22aa830d9c16dc54424048359079400217943884a05c3`
and original asset manifest SHA-256
`5109ab4009b427fd75a62d90620940130471b506d605f48e0ae4e2fc9f5c9ba6`.
It selects clear weather `0x81A`, hour 12, fog and image space together. The
latest renderer source and private research artifacts are preserved. This is
an explicit older-binary route, not a source rollback.

A fresh 1920×1080 windowless replay exits successfully and settles at frame 584
with no unavailable or pending GPU dependencies. Roof, wood, stone, ground,
fern, sapling and sky rectangles match the earlier screenshot pixel-for-pixel.
The entire image differs in 59,400 pixels, including the distant-mountain
rectangle; no whole-image identity is claimed. Shell syntax and launcher help
pass. An interactive player session was not started.

Further candidates must retain this checkpoint and be assessed against retail
references with their capture-state limits visible. Individually supported
equations remain research evidence when the combined image regresses. The
[central lighting spec](../specs/engine/lighting-and-shading.md) owns that
acceptance policy; the [evidence summary](skyrim-lighting-regression-recovery-20261010.json)
records the recovered inputs and replay differences.
