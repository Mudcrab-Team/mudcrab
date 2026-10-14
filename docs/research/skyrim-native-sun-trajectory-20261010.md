The saved full-mode Sun::Update closes Skyrim’s local sunlight trajectory. It does not place the light at zenith at noon. The renderer now uses this local trajectory while explicitly retaining an identity Sky parent in our preview. This establishes native local arithmetic; it does not establish a captured retail world direction.

The executable is Skyrim SE 1.7.104.0, SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. This report reuses the saved Sun::Update and Init exports. One additional saved-project query ran read-only with analysis disabled in 6.05 seconds. All 849 distinct exported instruction starts and all requested spans match the executable bytes. No executable, renderer or GPU session ran for this audit.

Sun::Update `0x14041BD90` selects full mode when `Sky.mode` at `+0x1BC` is 3. It reads game hour from `Sky+0x1B0`. Dirty flags 9–12 in `Sky+0x1DC` refresh four cached endpoint hours from the active climate at `Sky+0x40`: TNAM bytes `+0x78/+0x79/+0x7A/+0x7B`, converted to binary32 and multiplied by binary32 `1/6` (`0x3E2AAAAB`, data at `0x141869DD8`). An absent climate preserves the existing cached endpoint. These original endpoints must not be replaced by extended weather-color transition boundaries.

The selected executable defaults are byte-checked alongside their setting-name pointers:

| Setting | Default | Data address | Binary32 bits |
|---|---:|---|---|
| fSunAlphaTransTime | 2 | 0x14209E948 | 0x40000000 |
| fSunXExtreme | 400 | 0x14209E960 | 0x43C80000 |
| fSunYExtreme | 25 | 0x14209E978 | 0x41C80000 |
| fSunZExtreme | -100 | 0x14209E990 | 0xC2C80000 |
| fSunDirXExtreme | 400 | 0x14209E9A8 | 0x43C80000 |

Live overrides of these settings are not captured. The full-mode branch contains no trigonometric angle calculation or 360-degree clamp. Its day length uses exact binary32 24 (`0x41C00000`, `0x14185C194`).

Every scalar operation below rounds to binary32 in the listed order. The double operation uses `q+q`, as the native instructions do:

```text
sunriseMid = (sunriseBegin + sunriseEnd) * 0.5
sunsetMid  = (sunsetBegin + sunsetEnd) * 0.5
halfAlpha  = fSunAlphaTransTime * 0.5
A = sunriseMid - halfAlpha
B = sunsetMid + halfAlpha

if A < h && h < B:
    q = (h - A) / (B - A)
    x = (1 - (q + q)) * fSunXExtreme
else:
    if h >= B: n = h - B
    else:      n = (24 - B) + h
    q = n / (24 - (B - A))
    x = ((q + q) - 1) * fSunXExtreme

r = -(fSunDirXExtreme / fSunXExtreme)
v = normalize([r*x, fSunYExtreme*r, fSunZExtreme])
```

Day comparisons at `0x14041C3CC` through `0x14041C3D6` are strict; exact A enters the night-before branch, and exact B enters night-after. Day arithmetic ends at `0x14041C43E`. Night-before numerator arithmetic is at `0x14041C413` through `0x14041C423`; night-after starts at `0x14041C3FB`. The vector producer is `0x14041C49D` through `0x14041C4DE`, with normalization at `0x1402B6540`: square and add x/y first, then z, square root, reciprocal, and component multiplication. Small-vector handling uses the threshold recorded in the JSON.

The normalized vector occupies the first column of NiDirectionalLight.local.rotate; writes are `0x14041C513/0x14041C528`. Constructor `0x140F04570` identifies vtable `0x141A4AF98`; slot 48, byte offset `0x180`, is the recovered world updater `0x140F045C0`. After base world update it copies the first world-rotation column at `+0x7C/+0x88/+0x94` to the light direction at `+0x140/+0x144/+0x148`. The existing Lighting consumer `0x14154B770` negates all three before upload. The normalized local vector is therefore the light-forward direction; shader toward-light uses its negated effective world direction.

Parent handling remains a stated boundary. SkyObject::Init `0x14041A210` creates a new NiNode for Sun.root and attaches it to the supplied Sky root at `0x14041A2CA`. Sun::Init attaches the directional light to Sun.root at `0x14041BABE`. The saved Sun routines do not rotate that parent. Base world update `0x140EEBF40` uses the parent transform when present. The caller’s effective Sky world rotation, runtime controllers, update timing and exceptional transform flags are not captured by this bounded trace. Our preview can state an identity parent; it must not report that choice as a captured retail parent transform.

The oracle uses original active CLMT bytes `[33,60,96,123]`, hours `[5.5,10,16,20.5]`, the selected defaults and an identity parent. It gives A=6.75, B=19.25. At h=12, native scalar order gives x=`64.00000762939453`. After negation and Creation-to-engine `(x,z,-y)` conversion, toward-light is `[0.5274865031242371, 0.8241975903511047, -0.20604939758777618]`. The JSON includes exact binary32 results for h=0,6,6.75,12,19.25,20,24. Those values are a static oracle, not retail capture measurements.

The sunlight RGB/dimmer result remains separate and unchanged: direct Sky RGB copy, default dimmer one, and the recovered SunlightScale consumer. This trajectory finding supplies no brightness or saturation coefficient.

The exterior implementation retains original CLMT bytes separately from extended weather-color intervals. Interior and unresolved-interior directions retain their earlier behavior. Six native direction oracles match binary32 component bits; all 454 engine library tests, strict Clippy and the quick engine build pass. Automatic and fixed-adaptation world captures isolate the direction change with sunlight RGB and authored grading unchanged. The change brightens the selected wood region while pale roof/stone hotspots persist; it is an input correction, not visual acceptance. The [coverage report](skyrim-native-diffuse-coverage-20261010.md) records those captures and the later full-pack texture-view comparison.
