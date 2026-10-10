# Apple Metal profiling evidence

These records preserve the final local captures from 8–9 October 2026 on an Apple M1 Pro running macOS 26.6.2. The [profiling guide](../../../contributing/native-profiling.md) describes the tools and capture protocol; the [investigation](../../metal-performance-fixes-20261008.md) explains the changes and remaining limits.

| Observation | Before | Final | Scope |
| --- | ---: | ---: | --- |
| Fixed-scene FPS | 47.315 | 47.453 | One fresh baseline versus the median of three final captures; +0.137 FPS does not establish a gain. |
| Fixed-scene sampled CPU cores | 1.411 | 1.177 | Guarded native CPU crops; endpoint attribution, with recognized waits reported separately. |
| Ordinary presented FPS | 48.448 | 59.971 | One capture per side; the +11.523 FPS observation needs repetition. |
| Static full-capture peak process RSS | 2.428 GiB | 2.770 GiB | Once-per-second samples include loading; +14.1%, not Metal allocation size. |

The fixed scene uses the same Riverwood raised camera and actual 3200×1802 Immediate surface. Ordinary play uses the normal camera/player path and actual 3200×1800 Fifo. These workloads remain separate. Shadows, reflections, terrain detail, GPU culling and validation remain enabled.

The unchanged [before PNG](before.png) is from `fixes-baseline-7`; the unchanged [after PNG](after.png) is from `fixes-final-v4-1`. Their pixel dimensions and camera match. All three final stationary images passed manual terrain review; only this pair is published here. These saved frames do not establish complete coverage during ordinary play or movement.

The final moving test records 60 origin rebases, 300 cell unloads and zero streaming-budget violations. It has no matched timing baseline or moving image oracle. Six model instances and two arming entries remain at stop. Its optional screenshot gate passes without a requested image; that is explicitly distinguished from an actual screenshot.

The separate draw-count capture observes 327 covered fixed indexed API submissions and 2,851 argument slots per schedule across 468 complete schedules. Its ten-second render-clock crop includes about one second of warmup. All eleven expected material mesh variants are wrapped, while three transparent gizmo variants remain unmeasured. Slots can have zero instances and do not count visible draws.

Download [dashboard.html](dashboard.html) and open it in a browser for the preserved FPS visual and final comparisons. Its plots and tables are embedded; raw-capture links refer to the original local archive. The result JSONs and screenshot pair in this folder provide the published evidence.

`manifest.json` records SHA-256 hashes and file sizes. The three result JSON files retain the timing windows, counters, sample counts, source provenance and qualifications. `runs/` contains the eight selected capture manifests, saved visual reviews and available export/count verification. Absolute paths inside those machine records identify the original local captures; they are not public download locations.

Raw native profiles, full symbolicated exports, binaries, assets, complete logs and historical experiments stay in the local archive. Offline exports retain loading and teardown; reproducing the summaries requires their recorded guarded windows. Firefox's default sample weights show residence, whereas these endpoint estimates use CPU deltas. All-zero GPU pass values do not establish active GPU work. Full Xcode's Metal timeline is still needed to separate GPU completion from compositor release during drawable waits.

Correctness is recorded in `correctness-v4.json`: 1,141 tests pass, zero fail and 18 remain ignored, with workspace Clippy warnings denied. No Windows or Linux runtime validation is claimed.
