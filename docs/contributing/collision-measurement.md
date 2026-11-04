# Fixed-container collision measurement

The `collision-measure` Cargo feature enables an experimental measurement tool. It is absent from normal builds. Set `MUDCRAB_COLLISION_MEASURE` to a writable CSV path to enable world physics and sample a run. Set `MUDCRAB_COLLISION_MEASURE_OFF=1` to exclude the audited container tier for a baseline. Keep all other settings identical. Do not write output into a registered pack.

Use a release build and a stationary streaming benchmark with `--streaming-benchmark-route-speed 0`. Alternate baseline and enabled runs. Apply the shared-storage rules and the host's native workload watchdog. The CSV `phase` is 1 during the settled tail. `colliders` is the live Rapier collider count. `steps` counts fixed simulation steps in the frame, and `step_ms` is their combined elapsed interval around `PhysicsSet::StepSimulation`. That interval can include scheduler overhead and other concurrent work. `frame_ms` is the real-clock frame delta. Restrict per-step distributions to frames with exactly one step. Check the report and watchdog receipt before interpreting results; a completed sample does not prove a clean process exit.

The feature forces interactive world physics in benchmark modes. It is for controlled measurement only, not a gameplay or acceptance option. Its output does not establish player contact, correct placement, or absence of blockers.
