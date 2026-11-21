# Combined culling review frame cost, 2026-11-17

This sample does not establish a reliable combined-stack speedup. The enabled median is lower,
but the ranges of whole-run means overlap. At the settled Mercer house viewpoint, five alternating
runs per mode give:

| Mode | Median mean frame time | Run range | Camera meshes |
| --- | ---: | ---: | ---: |
| Full rendering | 25.44 ms | 24.64–26.13 ms | 768 of 768 |
| Combined room culling | 24.00 ms | 23.18–24.92 ms | 403 of 768 |

Each run measures 600 frames after 300 warm-up frames, using a release executable built from
`4de561bef462ec1e956fad9c1a4c67387ae47ace` plus the same
[measurement-only patch](evidence/room-review-20261117/measurement-only.patch) used for the
[core-layer comparison](room-culling-review-20261117.md). The executable's SHA-256 is
`1b845509a6b83186f88477b2db25032304a71d7fc157481ce1a233c9f7d7c775`.
Both controls use that executable and identical geometry. The independent off database copy
omits room tables. The newly reconverted assets differ from the core campaign, so the two
campaigns do not isolate the performance cost of adding the upper layers.

The platform is Linux/NixOS, Ryzen 5 1600, GTX 1660, driver 595.112.07, Vulkan, native Wayland,
1400×900, with benchmark vsync disabled. No compiler or other test engine ran during timing.
All 6,000 measured frames passed renderer proof with GPU culling, indirect drawing, and HZB active.
Every capture settled without pending or failed work. Saved interactive preferences are ignored
by automated captures. The live room-culling toggle is verified separately by persistence and ECS tests.

Separate instrumented profiles place 15.33 ms enabled and 16.85 ms disabled in the render thread's
graph/presentation interval. Render preparation is about 6.3 ms. Main-world work is 6.19 ms enabled
and 5.28 ms disabled, while median GPU utilization is 39% in both. These profiles point to the host
render/presentation path as the limiter, with some additional main-world work in the enabled mode.
Native driver stacks were not sampled, so the cause inside that interval remains unresolved.
CPU phases and GPU pass times overlap and must not be added as a serial budget.

[All run values, profiles, hardware, and executable identity](evidence/room-review-20261117/combined-frame-times.json)
retain the evidence. This is one warmed stationary viewpoint. It does not measure cold loading,
continuous camera movement, original Skyrim's frame cost, another backend, or local occluders.
The 48 fresh visual captures and the matched retail oracle are in
[the authored review record](room-assignment-review-20261117.md), and the 34 boundary captures are
in [the room-gap review record](room-gap-review-20261117.md).

## Stored-bounds eligibility follow-up

The later unknown-bounds guard protects legacy geometric memberships. Authored named-target and
source-sphere controls preserve their independent ownership behavior. After propagation, the
combined engine passed 1,642 library tests with 11 ignored, formatting, and strict engine
library/test Clippy. Converter source is unchanged by this follow-up. The measurements and images
above remain pinned to their recorded executables and were not rerun for the eligibility guard.
