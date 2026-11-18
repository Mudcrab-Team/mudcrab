# Sun-shadow stepping

The game clock continuously evaluates the climate/GMST sun path. The directional
light transform keeps that direction, so illumination, weather colours and sky
updates remain continuous. Shadow cascades use an independent held rotation.
Only the marked world sun uses this policy; other lights retain Bevy's behavior.

Each frame, Bevy computes the ordinary cascades, then the sun-shadow system refits
the sun's cascades to the held basis before light frusta and caster visibility.
The refit preserves Bevy 0.19's camera-relative texel snapping, reverse Z, cascade
distances and overlap. It covers every active view, including reflection cameras.
Camera movement still refits cascades each frame; holding sun rotation prevents
clock-driven rotation of the entire texel grid while standing still. The copied
projection construction must be checked against Bevy when upgrading.

## INI contract

These keys are read from `[Display]` through `--ini`, with the reader's existing
case-insensitive, later-file-wins layering. Malformed/nonfinite values warn and
retain the prior configuration.

| Key | Default | Engine behavior |
| --- | --- | --- |
| `fSunUpdateThreshold` | `0.5` | Snap when the angle between the held and current sun directions reaches this value. **Degrees are an engine choice; retail's threshold units are unverified.** Accepted range: 0–180; zero follows every direction change. |
| `fSunShadowUpdateTime` | `1.0` seconds | Parse and report the retail transition/cross-fade duration. Cross-fading is a future follow-up: shadows currently snap as soon as the threshold is reached. A nonzero duration logs this limitation once at startup. Accepted values: finite, nonnegative. This value is reserved, with no scheduling effect. |
| `fSunShadowMaxHoldSeconds` | `10.0` virtual seconds | **Engine extension**, maximum hold while the game clock moves, even below the angular threshold. Accepted values: finite, strictly positive. Paused/frozen game time consumes none of this budget. |

The two legacy numeric defaults above are documented by the [STEP INI Display
reference](https://stepmodifications.org/wiki/Guide%3ASkyrim_INI/Display#fSunShadowUpdateTime)
and [default INI listing](https://stepmodifications.org/wiki/Guide%3ASkyrim_INI/Default_INI).
The default angular interpretation is a conservative engine policy, not a claim
of retail parity. The maximum-hold value is also an engine policy.

[STEP's original transition experiments](https://stepmodifications.org/forum/topic/9209-sun-shadow-transition-ini-settings/)
distinguish transition duration from spacing between steps. The documentation
reports about one second between transitions at threshold 0.05 and twenty at 1.0,
without establishing the internal threshold units. No exact-key definition was
found on UESP or the Creation Kit wiki. The local retail findings registry has no
entry for these settings.

This policy models legacy stepped shadows, rather than claiming every Skyrim
edition does this by default. [STEP's SSE investigation](https://stepmodifications.org/forum/topic/15233-shadow-optimization-for-sse/)
reports that SSE defaults to `bDisableShadowJumps=1`, a continuous mode that ignores
both legacy keys. That mode and shadow cross-fading are outside this change.

## Discontinuities and capture determinism

The first update, an explicit game-hour edit (in either direction), changed
worldspace/climate/GMST inputs, and an advance of at least fifteen game minutes in
one frame immediately synchronize the shadow basis. Ordinary midnight wrapping
continues to use the threshold/hold policy. Explicit `--sun-position` retains its
fixed illumination and shadow direction.

Shots, acceptance captures and benchmarks already freeze the game clock. They
initialize shadows directly at the requested hour and keep the same projection
independent of streaming settle time. Freezing an interactive clock also keeps
its held shadow basis. Shadow biases, map resolution and filtering retain their
existing settings. Visual validation belongs to the user's live test.

## Regression contract

Headless ECS regressions use the production sun-shadow plugin with Bevy's cascade
and frustum systems. They cover held matrices with moving illumination, threshold
and maximum-hold updates, immediate time/path changes, frozen capture equality,
camera refitting, light-setting preservation, and parity with Bevy's projection
construction when the two directions match. INI regressions cover layering and
invalid input. Executing these tests and recording red/green evidence follows
the implementation audit.

## Bevy projection construction license

The adapted cascade-fitting function in `sun_shadow.rs` uses Bevy 0.19's MIT
license option. The original permission notice follows:

```text
MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
