# Independent jump evidence design

The goal is native gravity, launch, apex and timestep semantics with reproducible held-out evidence, without changing Mudcrab physics.

The three design sketches converged on an offline evaluator with separate freeze and score commands. Candidate B supplies the base. Predictions accept clock arrays and initial conditions, never future observed height or velocity. Candidate A contributes launch uncertainty and separate directional curvature. Candidate C contributes a test that changes future observations and demands unchanged predictions. No live fitter or general plugin framework is needed.

Selection criteria are leakage prevention, honest clock/phase semantics, falsifiable native evidence, reproducibility and size of interface. All three meet the first four. B states the smallest explicit forecast boundary. The arena cross-judge is skipped because every configured pool entry shares the parent model family and other-family providers are disabled. This limitation is not evidence of cross-model agreement.

Native instruction inspection after the sketches found a conditional fall multiplier and a jump state that derives impulse from gravity and height. This makes direct state-update observations more useful than more position-only fits. Recorder v3 extends #520 with camera-boundary controller snapshots and before/after airborne/jump state callbacks. These preserve original execution and are checked against the pinned executable. They must prove their pointer/type and behavioral meaning in a pilot before calibration.

Data shapes are Run (provenance, input events, samples, native state updates), FrozenModel (training hashes, source hash, laws and parameters), ForecastInput (times only and launch origin) and Report (observed facts, inferred parameters, failures and unknowns). The native state measurements diagnose physics semantics separately from the position forecast. Camera spring velocity is never player velocity.

Old #520 jumps and new v3 pilots are development data. Three fresh 60 Hz captures calibrate. Freeze coefficients and scoring rules before capturing nine held-out runs, three each at requested 30 Hz, 120 Hz, and 60 Hz with a 0.5 game-time multiplier. Report achieved cadence. Run one game at a time in a private prefix. Preserve failed attempts. No held-out result licenses tuning or production changes.
