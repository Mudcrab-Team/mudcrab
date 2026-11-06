# Shipped image-space arithmetic

The selected retail package contains the complete arithmetic for the programs below. This map covers 54 groups, 108 serialized stage records and 54 distinct bytecodes. Every selected key is `00000000`. The companion [equation catalog](image-space-arithmetic.json) contains a component equation for every arithmetic instruction, every sample, every output write and every control-flow edge, including the vertex programs. It covers 1,821 executable instructions across the distinct bytecodes.

This establishes package behavior given shader inputs. It does not establish which programs run in a retail frame, their pass order, the meaning or color domain of their inputs, the selected surface formats, or the native writers of their constants. Native names and section associations come from the frozen [loader proof](native-shader-loader.json); names do not supply missing equations.

## Evidence and notation

The source package is `shadersfx/shaders011.fxp`, SHA-256 `72cfc73eb0938d5949f0b3a8683722a33b671a34b41b750ba138de8d154c5fbc`. All 108 scoped records were compared with the original package bytes. An independent token reader consumed all operand boundaries for all 54 distinct programs, then compared 4,784 operands and 1,440 literal components with the fresh disassembly. The checks also cover source modifiers, resource swizzles, write masks, 1D/2D/3D sample dimensions, explicit LOD and the 11-vector immediate table in `ISSnowSSS`. No scoped sample contains an instruction-level integer texel offset; shader-computed coordinate offsets remain in the equations.

Instruction ranges below use one-based executable instructions, excluding declarations and immediate-table data. The JSON pins each range to the full bytecode hash and its byte span. Original bytes and disassembly stay in private research storage.

`u = v1.xy`. `Tn(p)` means an implicit-derivative sample of texture slot `tn` through sampler `sn`; `TnL(p,l)` supplies the stated LOD. Each returns the original sampled vector before the program's resource swizzle. Texture addressing, filtering, SRV conversion and surface domain remain unknown. Samples occur at their original control-flow location. [Microsoft sample semantics](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/sample--sm4---asm-), [explicit LOD semantics](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/sample-l--sm4---asm-).

`M(a,b,c)` is one D3D multiply-add operation, including componentwise vector forms. It is retained as an operation rather than replaced with a chosen CPU rounding model. `Dk(a,b)` is the shader's k-component dot product. `f(d)` rounds a decimal literal to the exact binary32 value; the JSON's `literal_bits32` field is authoritative. Ordinary `+`, `-`, `*` and `/` below denote the corresponding separate shader operations. Rearranging them is not a claim of bitwise equivalence. D3D permits implementation variation in floating arithmetic, multiply-add, dot products and division. [Multiply-add](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/mad--sm4---asm-), [dot product](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/dp3--sm4---asm-), [floating-point rules](https://learn.microsoft.com/en-us/windows/win32/direct3d11/floating-point-rules), [division](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/div--sm4---asm-).

`sat(x)` is the destination saturation modifier, applied after the operation. Its NaN result is zero. `min` and `max` retain D3D handling of NaNs and denormals. `log2` and `exp2` retain their separate shader operations, including zero, negative and nonfinite behavior; they are not replaced with an unspecified `pow`. [Saturation](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/saturate), [minimum](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/min--sm4---asm-), [maximum](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/max--sm4---asm-), [logarithm](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/log--sm4---asm-), [exponential](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/exp--sm4---asm-).

The recurring coordinate transformation is:

```text
C(p).x = min(max(p.x * cb12[43].x, 0), cb12[44].z)
C(p).y = min(max(p.y * cb12[43].y, 0), cb12[43].y)
L = [f(0.2125), f(0.7154), f(0.0721)]
```

The bounds are shader inputs. This is not an assumption that they equal one. The RGB weighting `L` appears in the source instructions; its physical meaning and the sampled numeric domain remain separate questions.

## Groups 26–27: tone blend and fade

For `ISHDRTonemapBlendCinematic`, instructions 1–17 obtain three inputs and select a coordinate path:

```text
A = T1(C(u)).xyz
B = T0(C(u)).xyz if 0.5 < cb2[0].x else T0(u).xyz
P = T2(u).xy
Y = max(D3(L,A), f(0.00001))
R = P.y / P.x
Z = Y * R
choose_first = 0.5 < cb2[2].z
```

Both scalar alternatives are calculated before the component selection. Instructions 18–31 preserve the following operation order:

```text
a = M(R,Y,-f(0.004))
denominator = M(R,Y,1)
a = max(a,0)
b = M(a,f(6.2),0.5)
c = M(a,f(6.2),f(1.7))
n = a * b
d = M(a,c,f(0.06))
h = n / d
h = log2(h)
h = h * f(2.2)
h = exp2(h)
first = h * cb2[2].y

e = M(Z,cb2[2].y,1)
e = e * Z
second = e / denominator
Q = first if choose_first else second
```

Instructions 32–43 combine the inputs and modify all four components. Scalar additions and products broadcast to every stated component:

```text
scale = Q / Y
mix = sat(cb2[2].x - Q)
B = B * mix
rgb = M(A,scale,B)
K = D3(rgb,L)
V = [rgb.x,rgb.y,rgb.z,1]
V = V - K
V = M(cb2[3].x,V,K)
D = M(K,cb2[4].xyzw,-V)
V = M(cb2[4].w,D,V)
V = M(cb2[3].w,V,-P.x)
V = M(cb2[3].z,V,P.x)
```

The dot-product operand order differs between instructions 13 and 36; the complete equations retain it. Instructions 44–49 finish the program:

```text
V.xyz = sat(V.xyz)
H = log2(V.xyz)
H = H * cb12[42].x
o0.xyz = exp2(H)
o0.w = V.w
return
```

`ISHDRTonemapBlendCinematicFade` has the same operation sequence through the final power chain, then replaces the output writes with a four-component fade at instructions 47–50:

```text
V.xyz = exp2(H)
D = cb2[5].xyzw - V
o0.xyzw = M(cb2[5].w,D,V)
return
```

The fade therefore occurs after the power chain, includes alpha, and has no additional output saturation. These instructions alone do not identify `cb12[42].x` as a display transfer exponent or identify the `cb2` fields as authored image-space parameters. Those names require their CPU producers.

## Groups 28–35: downsample and adaptation

Each program uses a loop with a signed integer counter, beginning at zero. The fixed sample count is four or sixteen. Its recurring coordinate and weight are:

```text
p_i = M(cb2[i+7].xy,cb2[6].xy,u)
p_i = C(p_i) if 0.5 < cb2[0].x else p_i
w_i = cb2[i+7].z
```

The accumulator update is ordered by increasing `i`; it is not an unordered mathematical sum.

| Groups | Count | Per-sample input | Ordered accumulator update | Output |
| --- | ---: | --- | --- | --- |
| 28 `HDRDownSample16`, 29 `HDRDownSample4`, 33 `HDRDownSample4LumClamp` | 16 / 4 / 4 | `T0(p_i).xyz` | `A.xyz = M(sample.xyz,w_i,A.xyz)`, initially zero | `[A.xyz,cb2[6].z]` |
| 30 `HDRDownSample16Lum`, 35 `HDRDownSample16LumClamp` | 16 | `T0(p_i).x` | `A.xyz = M(sample.x,w_i,A.x)`; scalar addend broadcasts | `[A.xyz,cb2[6].z]` |
| 31 `HDRDownSample4RGB2Lum` | 4 | `D3(L,T0(p_i).xyz)` | `A.xyz = M(value,w_i,A.x)`; scalar addend broadcasts | `[A.xyz,cb2[6].z]` |
| 32 `HDRDownSample4LightAdapt` | 4 | `T0(p_i).xyz` | Independent RGB accumulators as group 29, then adaptation below | `[adapted.xy,A.z,cb2[6].z]` |
| 34 `HDRDownSample16LightAdapt` | 16 | `T0(p_i).x` | Broadcast scalar accumulator as group 30, then adaptation below | `[adapted.xy,A.z,cb2[6].z]` |

All names in this table have the `IS` prefix in the catalog. Group 30/31/34/35 initialization writes `[0,f(9999999),-f(9999999)]`, but the first accumulator update reads only its zero-valued first component and broadcasts the addend. After that update the three accumulator components have the same value. The sentinel initialization does not prove a running minimum or maximum.

Group 33 is byte-identical to group 29. Group 35 is byte-identical to group 30. The programs named `LumClamp` introduce no extra luminance clamp in this package. The recurring min/max instructions clamp coordinates.

Groups 32 and 34 share instructions 20–36 for the adaptation output:

```text
P = T1(u).xy
bad = any(A.c != A.c for c in x,y,z)
delta = A.xy - P
rate = delta * [cb2[2].w,cb2[2].z]
sign = float32((0 < rate) ? 1 : 0) - float32((rate < 0) ? 1 : 0)
bound = max(abs(rate),f(0.00390625))
step = min(abs(delta),bound)
candidate = M(step,sign,P)
o0.xy = P if bad else candidate
o0.z = A.z
o0.w = cb2[6].z
```

The sign expression abbreviates two comparison masks, two's-complement integer arithmetic and signed integer-to-float conversion retained in the JSON. Comparisons return `0xffffffff` or zero. The `bad` test uses three float self-inequalities and two bitwise ORs; a NaN causes the previous sampled `xy` to be selected. It does not replace `z`. [Float inequality](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/ne--sm4---asm-), [integer addition](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/iadd--sm4---asm-), [signed conversion](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/itof--sm4---asm-).

## Groups 36–56 and 78: blur and bright pass

The fixed filter sizes are `N = 3,5,7,9,11,13,15`, in group order. A filter iteration uses:

```text
p_i = u + cb2[i+3].xy
p_i = C(p_i) if cb2[1].x < 0.5 else p_i
w_i = cb2[i+3].z
```

This branch condition is the reverse of the downsample condition and reads a different constant. Offsets and weights are supplied by the constant buffer; this evidence does not prescribe Gaussian weights or pixel spacing.

Groups 36–42, `ISBlur3` through `ISBlur15`, accumulate at instructions 6–25:

```text
S = T0(p_i).xyzw
S = S * [cb2[1].w,cb2[1].w,cb2[1].w,1]
A = M(S,w_i,A), initially [0,0,0,0]
o0 = A * [cb2[1].z,cb2[1].z,cb2[1].z,1]
```

The output is at instruction 28. Neither scale multiplies alpha, but alpha participates in the weighted accumulation. Groups 43–49, `ISNonHDRBlur3` through `ISNonHDRBlur15`, use the same offset/weight convention and accumulate raw sampled RGBA, without either RGB scale. Their output is instruction 20. Group 49 is a `NonHDRBlur` program, rather than a bright-pass variant.

Group 78, `ISBlur`, has the same arithmetic as the NonHDR family except that the loop bound is the unsigned bit interpretation of `cb2[2].x`. It does not convert a float sample count to an integer.

Groups 50–56, `ISBrightPassBlur3` through `ISBrightPassBlur15`, first calculate output alpha at instructions 1–3, then accumulate RGB at instructions 5–27:

```text
alpha = D3(T1(u).xyz * cb2[1].w,L)
A.xyz = [0,0,0]
for each i in increasing order:
    S.xyz = T0(p_i).xyz * cb2[1].w
    S.xyz = S.xyz - cb2[0].x
    S.xyz = max(S.xyz,0)
    S.xyz = S.xyz * cb2[0].y
    A.xyz = M(S.xyz,w_i,A.xyz)
o0.xyz = A.xyz * cb2[1].z
o0.w = alpha
```

The output write is instruction 30. Alpha comes from a separate center sample of `t1`; it is not the blur sum, a coverage constant, or a thresholded value. There is no destination saturation on this output.

## Groups 98–100: lighting composites

All texture samples in these three pixel programs use `u` without the recurring coordinate transform. Set `Sn = Tn(u).xyzw`.

Group 98, `ISLightingComposite`, builds `B` in instructions 1–12:

```text
V = S5 * S7.x
V = M(S1,S3.x,V)
D = S6 * S7.x
D = M(S2,S3.x,D)
B = M(V,S0,D)
```

Group 99, `ISLightingCompositeNoDirectionalLight`, builds its four-component value in instructions 1–7:

```text
V = S1 * S3.x
D = S2 * S3.x
B = M(V,S0,D)
```

Group 100, `ISLightingCompositeMenu`, builds it in instructions 1–8:

```text
V = S1 + S5
D = S2 + S6
B = M(V,S0,D)
```

Each then prepares the same optional output. The exact order of samples and scalar addition is retained per program in the JSON:

```text
O = S4
V.xyz = O.xyz - B.xyz
V.xyz = M(O.w,V.xyz,B.xyz)
V.xyz = sat(V.xyz * cb2[1].w)
V.w = sat(B.w)
sum = O.y + O.x
sum = O.z + sum
sum = O.w + sum
o0 = V if sum != 0 else B
```

The scalar RGB scale and saturation affect the selected `V` path. When the sample-component sum equals zero, the program outputs the unsaturated `B` instead. That distinction must survive a port; replacing the selection with an unconditional scale/clamp changes the program. A NaN sum passes the float `!= 0` condition. The source instructions identify slots, not whether a texture is albedo, ambient, directional light or a decal.

## Groups 111–113: SAO and fog composites

These programs combine a sampled RGBA value with several gated additions, a procedural coordinate function and a final four-component saturation. Group 112 contains the fog transfer at instructions 31–50; group 113 contains it at 38–57. The full equation tree includes the complete procedural function, not an assumed replacement noise implementation.

The recurring inputs use `p = C(u)`. Begin with `B = T0L(p,0)`. When `0.5 < cb2[10].z`, sample `J = T3L(p,0).zw` and `V = T6(p)`. If `J.x <= f(0.00001)` and `J.y > f(0.00001)`, add this bounded RGB term:

```text
R = V.w * V.xyz
R = R * cb2[10].x
cap = B.xyz * cb2[10].y
R = min(max(R,0),cap)
B.xyz = B.xyz + R
```

If that inner condition is false the addition is zero. If the outer condition is false the samples and inner computation do not execute.

Set `enabled = cb2[5].w != 0`. If enabled, sample `m = T4(p).y` and `k = T4(p).x`, then `B.xyz = M(k,m,B.xyz)`. Otherwise `m = 0`. Here `m` and `k` are sampled channels, without inferred physical names.

Groups 111 and 113 additionally sample `a = T1(p).x`, calculate `b = min(a + cb2[8].x,1)`, and select `b` when both `enabled` and `f(0.00001) < m`; otherwise they retain `a`. Group 111 replaces `B.xyz` with `a * B.xyz`. For group 113, `B` in the fog equations below denotes RGB before this multiplication; `scaled = a * B.xyz` names the multiplied value. Group 112 omits this sample and multiplication.

For the fog transfer, sample `d = T2L(p,0).x` and retain each step:

```text
r = M(d,f(1.01),-f(0.01))
r = M(r,2,-1)
A = cb2[3].x
D = cb2[3].y
numerator = D2([A,A],[D,D])
sum = A + D
difference = D - A
denominator = M(-r,difference,sum)
r = numerator / denominator
r = sat(M(r,cb2[0].y,-cb2[0].x))
r = log2(r)
r = r * cb2[0].z
r = exp2(r)
F = min(r,cb2[0].w)
color_delta = cb2[2].xyz - cb2[1].xyz
color = M(F,color_delta,cb2[1].xyz)
```

There is no division by `F` and no generic density constant introduced here. `A` and `D` remain actual buffer inputs; describing them as near/far projection values needs the CPU producer. The two-component dot product is preserved rather than replaced with a selected rounding sequence for `2*A*D`.

In group 112 the RGB path is:

```text
delta = color - B.xyz
fogged = M(F,delta,B.xyz)
fogged = fogged * cb2[1].w
B.xyz = fogged if d < f(0.9999989867210388) else B.xyz
```

Group 113 uses the selected `a` scalar and retains a different multiply-add grouping:

```text
scaled = a * B.xyz
delta = M(-B.xyz,a,color)
fogged = M(F,delta,scaled)
fogged = fogged * cb2[1].w
B.xyz = fogged if d < f(0.9999989867210388) else scaled
```

In both cases `B.w` retains the original sampled alpha through these RGB operations. These equations establish that the fog composites can change brightness as well as blend RGB. They do not establish that either program executes before or after the tone programs in the scene being tested.

All three composites contain a further gated procedural path. Its enable condition is `enabled`, `m != 0`, and `f(0.00001) < cb2[7].z`. The path uses the already sampled `d` in groups 112/113 and samples it inside the path in group 111. It samples `x = T5L(p,0).x` and builds:

```text
clip = [M(M(u.x,1,0),2,-1), M(M(u.y,-1,1),2,-1), d, 1]
H.xyz = [D4(cb12[32],clip),D4(cb12[33],clip),D4(cb12[34],clip)]
H.w = D4(cb12[35],clip)
P = H.xyz / H.w
P = P + cb12[40].xyz
```

The exact equations preserve the nested multiply-add generation of both clip components. The procedural block operates on `P * cb2[7].x`: group 111 instructions 55–155, group 112 instructions 67–167, and group 113 instructions 74–174. It includes all floor operations, component permutations, comparison masks, three multiply/floor/reduction chains with `289`, a `49`/`7` reduction, gradient-component reconstruction, normalization factors, four squared-distance weights and a final dot product. The negative-zero literal `0x80000000` is preserved. No library noise function is substituted. See these exact ranges in `programs_by_bytecode_sha256[hash].equations`.

Call the block's final scalar `N`. The common tail computes:

```text
K = sat(reciprocal(N))
D = P - cb2[5].xyz
distance = sqrt(D3(D,D))
inverse_bound = 1 / cb2[8].y
t = sat(inverse_bound * distance)
s = M(t,-2,3)
t = t * t
falloff = M(-s,t,1)

delta = 1 - x
gain = M(delta,cb2[9].x,x)
q = K * N
q = log2(abs(q))
q = q * cb2[6].z
q = exp2(q)
below = q < cb2[6].w
q = q * cb2[6].x
q = 0 if below else q
gain = gain * q
gain = falloff * gain
```

When the gated path does not execute, `gain = 0`. Every program then finishes with the following four-component result:

```text
scale = 1 - cb2[7].w
addition.w = m * gain
addition.xyz = addition.w * cb2[9].yzw
o0 = sat(M(B,scale,addition))
```

Group 111 uses the RGB after its scalar multiplication for `B`; groups 112/113 use their selected fog RGB. The scalar scale also multiplies sampled alpha, and the calculated addition also contributes to alpha. Saturation occurs after the final multiply-add on all four components.

## Other covered outputs

The equation catalog covers every instruction in each of these programs. This table identifies their output structure without assigning resource roles.

| Group | Interpreted behavior and exact range |
| --- | --- |
| 12 `ISCopyScaleBias` | Instructions 1–6 sample `t0/s0` at `C(u)` and return RGBA; this pixel program contains no additional color scale or bias. |
| 25 `ISCopyGreyScale` | Instructions 1–8 sample `S=T0(C(u))`, calculate `g=D4(S,cb2[1].xyzw)`, then output `[g,g,g,S.w]`. Its resource and temporary swizzles reconstruct original RGBA for the dot product. |
| 69 `ISAlphaBlend` | Instructions 1–82 sample 16 texture/sampler pairs in slot order. Their constants and sequential RGB multiply-adds are explicit in the JSON; output alpha is assigned separately. This is shader arithmetic, not an inferred output-merger blend state. |
| 74 `ISVolumetricLighting` | Instructions 1–3 output `[cb2[15].xyz*cb2[15].w,cb2[15].w]`, without a texture sample. |
| 75 `ApplyReflections` | Instructions 1–21 sample `t1.zw` at explicit LOD zero and `t0/t2` at `C(u)`. If `t1.z<=f(0.00001)` and `t1.w>f(0.00001)`, add `min(max((t0.w*t0.xyz)*cb2[0].x,0),t2.xyz*cb2[0].y)` to `t2.xyz`; otherwise return `t2.xyz`. The program writes only RGB. |
| 76 `ISApplyVolumetricLighting` | Instructions 1–44 combine a 2D sample, an explicit-LOD 1D sample and an explicit-LOD 3D sample, plus a scaled coordinate sample from `t4`. A gated path adds two sampled coordinate components, transforms them with `cb12[43].zw`, tests the original coordinates against `[0,1)`, and weights a difference against another 2D sample. It writes only `o0.x`. All resource-channel selections, thresholds and polynomial operations are in the full equations. |
| 77 `ISBasicCopy` | Instructions 1–2 output `T0(u).xyzw`. |
| 87 `ISCompositeVolumetricLighting` | Instructions 1–7 output `T0(C(u)).x*cb2[0].xyz`, RGB only. |
| 88 `ISCompositeLensFlare` | Instructions 1–3 output `T1(u).xyz`, RGB only. |
| 89 `ISCompositeLensFlareVolumetricLighting` | Instructions 1–8 output `M(cb2[0].xyz,T0(C(u)).x,T1(u).xyz)`, RGB only. |
| 92 `ISDownsample` | Instructions 1–13 select the first strictly greatest positive `D3(sample.xyz,[f(0.3),f(0.59),f(0.11)])` among samples at `M(cb2[i+1].xy,cb2[0].xy,u)`. The count is uint bits in `cb2[0].z`; initial RGBA and greatest value are zero. Instructions 14–27 optionally average this result with a `t1` sample displaced by `T2(C(u)).xy`, controlled by whether any bits of `cb2[0].w` are set. |
| 93 `ISDownsampleIgnoreBrightest` | Instructions 1–17 add all `T0(C(M(cb2[i+1].xy,cb2[0].xy,u)))` RGBA samples and divide by `float32(uint_bits(cb2[0].z))`. The program contains no brightest-sample rejection. The zero-count case is not guarded. |
| 96 `ISExp` | Instructions 1–5 multiply `T0(u).x` by `cb2[0].x`, multiply by `f(1.44269502)`, then apply `exp2`, broadcasting the scalar to RGBA. |
| 97 `ISIBLensFlares` | Instructions 1–40 use a signed loop from `-int_bits(cb2[1].x)` through `int_bits(cb2[1].x)`, inclusive. Samples are offset along x by `float32(i)*cb2[2].x`. The branch on `0.5<cb2[3].x` selects coordinate clamping. A positive sum of saturated RGB-minus-`cb2[0].z` gates the saturated full RGBA sample by bitwise masking. The sum is multiplied by `cb2[3].y` for output. |
| 119 `ISSILComposite` | Instructions 1–4 output `T0(u).xyzw + T1(u).xyzw`. |
| 123 `ISSnowSSS` | Instructions 1–45 contain a fixed 11-vector, RGB-dependent sample filter and a gated early RGB return. The full filter is described below; shader availability does not establish active subsurface scattering for other materials or the tested retail frame. |

## Group 123: the shipped SnowSSS filter

`ISSnowSSS` samples `m=T2(C(u)).y` and `B=T1L(C(u),0).xyz`. If `m<=f(0.00001)`, it writes `B` and returns at instructions 8–11. It does not write alpha on either path.

The remaining path samples `z=T0L(C(u),0).x`. Let `I[i]` be the exact immediate-table vector, whose 44 component bit patterns and float views are in the JSON. Instructions 13–18 initialize:

```text
A = B * I[0].xyz
width.xy = cb2[1].x * [f(0.078125),f(0.13889)]
width.xy = width.xy / z
depth_scale = cb2[1].y * f(0.1)
```

For table indices `i=1` through `10`, the loop at instructions 19–35 executes:

```text
p = M(I[i].w,width.xy,u)
S = T1L(C(p),0).xyz
d = T0L(C(p),0).x
q = z - d
q = abs(q) * depth_scale
q = min(q,1)
delta = B - S
S = M(q,delta,S)
A = M(I[i].xyz,S,A)
```

The initial RGB multiplier and `I[0].xyz` have identical bits in this package. The table supplies each RGB weight and scalar offset. No common RGB weight, Gaussian replacement or implicit mip selection is assumed.

Instructions 36–45 then compute:

```text
l = D3(A,[f(0.3),f(0.59),f(0.11)])
V = cb2[0].w * cb2[0].xyz
D = M(-cb2[0].xyz,cb2[0].w,B)
V = M(l,D,V)
V = max(V,0)
D = A - V
V = M(l,D,V)
D = V - B
o0.xyz = M(m,D,B)
```

This is exact package evidence for a specialized SnowSSS program. Material classification, its CPU constants, required source textures, scheduling and live activation remain separate native/runtime edges.

## State model and remaining boundaries

The JSON uses mutable component state `qN.xyzw` for shader temporaries and actual output slots `oN.xyzw`. All source components in one assignment are read before any destination component changes. Unwritten components remain unchanged; initial temporary and output values are undefined. This matters when a sample overwrites the same temporary that supplied its coordinate, when a dot product overwrites one of its inputs, and when alpha carries a separate result.

Its nested control tree retains if/else placement, local loop breaks and whole-program returns. Resource swizzles are applied by destination component position, not by packing the written components together. An unmodified move preserves bits. A `movc` predicate tests whether any bits are set; a floating comparison yields an all-bits mask, not integer one. Floor and integer conversion remain different operations. [Move](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/mov--sm4---asm-), [conditional move](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/movc--sm4---asm-), [floor](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/round-ni--sm4---asm-), [integer comparison](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/ige--sm4---asm-), [unsigned comparison](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/uge--sm4---asm-).

The remaining work is to connect each referenced constant component to its writer, each sampled slot to its producer and SRV/sampler descriptor, and these programs to an observed frame's selected keys, targets, blends and pass order. The package's equations do not justify changing Mudcrab brightness, saturation, fog density or transfer functions by eye. No renderer parameter or implementation was changed for this map.

## Pixel-program identities

The table below pins the readable contracts to exact bytecodes. The JSON also records every vertex program, every serialized record offset and each instruction's byte range.

| Group | Native catalog label | Pixel instructions | Pixel bytecode SHA-256 |
| ---: | --- | ---: | --- |
| 12 | `ISCopyScaleBias` | 6 | `79caf718f2dd1a80c4347a50c0223287931fa03b54f80b5517a1a28aeee5048e` |
| 25 | `ISCopyGreyScale` | 8 | `dfc098566ce05bf5cb826013dbc71033dc74388d9f64e8adbafa1a184b0edc26` |
| 26 | `ISHDRTonemapBlendCinematic` | 49 | `1def29cca7e582b149a09bd15524793593293ae2d94fc20b3d030bf9f91e39fc` |
| 27 | `ISHDRTonemapBlendCinematicFade` | 50 | `781978201a0b01ba4107b11abbe52456b5f20b49b998f1d0ba7aa9d057133ea1` |
| 28 | `ISHDRDownSample16` | 22 | `3f7ef23e31d2c07d1326654a7943da2e5ddfb28fb8a89b530f20dec8318baa05` |
| 29 | `ISHDRDownSample4` | 22 | `1abfac94c1717bd663ede710a11b5532384f1183dd11a141f9ac3aebb031d733` |
| 30 | `ISHDRDownSample16Lum` | 22 | `1e13dda0aeb8fdcd435b8d682ff2b9fa97fe670fd2d3cd382430fa8a591f6519` |
| 31 | `ISHDRDownSample4RGB2Lum` | 23 | `8c90ec91eb391f64b73cea0d3335cdbc92189ea4ae06c08727d193c8c69d1825` |
| 32 | `ISHDRDownSample4LightAdapt` | 36 | `f290f4ff09e163bb5f574824d4407b81d44a1fea47855f5325522e7b505f8826` |
| 33 | `ISHDRDownSample4LumClamp` | 22 | `1abfac94c1717bd663ede710a11b5532384f1183dd11a141f9ac3aebb031d733` |
| 34 | `ISHDRDownSample16LightAdapt` | 36 | `675f8c78eab2aa9a0877d6a55185d82911a66b7fc91569dc1331e8916ee2adda` |
| 35 | `ISHDRDownSample16LumClamp` | 22 | `1e13dda0aeb8fdcd435b8d682ff2b9fa97fe670fd2d3cd382430fa8a591f6519` |
| 36 | `ISBlur3` | 29 | `e0fcd2736b37e57e1605a58728fdb5a8296068f949d769d002b3093d9623a7cf` |
| 37 | `ISBlur5` | 29 | `06baab118ce8860f7fc8db5cca431711bd73610dd652be4614a497867b0dba4c` |
| 38 | `ISBlur7` | 29 | `34ef0156d7a97fb14217ff031200560006ae3e0b8cf5e804126b82ba2862703f` |
| 39 | `ISBlur9` | 29 | `1f9cf15745f63f57ab92e1ad47fe4cd32839eb0b933c0bf5cd9a763a538a8cc3` |
| 40 | `ISBlur11` | 29 | `c5d991b1f48cdfff90785d339854bdfb622c1f1b9407a4703302bb22da857d05` |
| 41 | `ISBlur13` | 29 | `6907f081e7dd117cf4f93e064e0394967cf26af7abcea6b5a45d7335d86cdac9` |
| 42 | `ISBlur15` | 29 | `9f756b5e779e1352643321c206387c74d126b016f3b5b4db9e864116b92351d6` |
| 43 | `ISNonHDRBlur3` | 21 | `1707da90531d63debd93b81a2cb334e1cdf42674c3a0c49c7597cd76823c23d5` |
| 44 | `ISNonHDRBlur5` | 21 | `1f2e4c04119ba18fe230423b8960e9cce19f09437b853fbdcf9c3aa0bfea1fcc` |
| 45 | `ISNonHDRBlur7` | 21 | `e2aa4afde539acae17dfe9d26456c85b14d93db535c9461b452b2c00fc3a8907` |
| 46 | `ISNonHDRBlur9` | 21 | `a05dfb02472d545ba647bfc3501ccc1d811936d31ab22a204bd6d8ac801e6a37` |
| 47 | `ISNonHDRBlur11` | 21 | `c62a9fb82cba0a162ff0284943fac3e5714ca64b486dab0a979dcffdb32b2c36` |
| 48 | `ISNonHDRBlur13` | 21 | `1da52d0f0f090542c54972f917da6f86f8d2caa8e41694ea37c00244057e8996` |
| 49 | `ISNonHDRBlur15` | 21 | `6b821c16446fdb96d86395d75e34bf87862daca5ff180aa0b4d4bfa660d6da1f` |
| 50 | `ISBrightPassBlur3` | 31 | `52ca43062521325217108af7285d71810fb780693c9adb8b4ede39351b739375` |
| 51 | `ISBrightPassBlur5` | 31 | `3eab75009e54616e8b739e8bb8f037a244f6b0b96b563f697ff75d5a46d285dd` |
| 52 | `ISBrightPassBlur7` | 31 | `1b78f2c3d25d7b516dbb163cc535531baa922e8fa3e444c6b26089edc0e53a77` |
| 53 | `ISBrightPassBlur9` | 31 | `1f812a01fbac3a4bbec88c70fa97c8b6d66aeba0baaf759a652eaf5a233d6c3a` |
| 54 | `ISBrightPassBlur11` | 31 | `6a66612bd620e38005da9d5915e008725618a467cc48f3519d6f49ab2c9bd205` |
| 55 | `ISBrightPassBlur13` | 31 | `320199c9c0a72ec3d76d31c6085ae4ba466b683f359fe1716382470531aa13d4` |
| 56 | `ISBrightPassBlur15` | 31 | `34bf3284b2444c1dfee81a6ea370bb201186763d9739dbb1576e219869fa8cbe` |
| 69 | `ISAlphaBlend` | 82 | `33a615a97bd3b2179d97b0849dd6d667ef3f020eff25f4bb5eca9554c6030461` |
| 74 | `ISVolumetricLighting` | 3 | `85a099f17f683c022a5fad097b3bb37eeeeb7550715d329d2fa8122c75d9f99e` |
| 75 | `ApplyReflections` | 21 | `e9c81aecda7340d5744037306842c030186b18a6d7329ea82aa351e25d194741` |
| 76 | `ISApplyVolumetricLighting` | 44 | `7e2dc05eb2839c4a7043bd596b57e365315c6739208702ead1a9bdd961fd6443` |
| 77 | `ISBasicCopy` | 2 | `622b98f7c48ed59f924f1ffdff5948acb710fca80f08ceb09b9c5903be6d73f4` |
| 78 | `ISBlur` | 21 | `0a71462a0531370828bc678af87988f3b57b0786687cc42af20895d14890ac37` |
| 87 | `ISCompositeVolumetricLighting` | 7 | `bf604d2bc1744dfb4f05e602784cc124e047fcf811ce0d01727df32aef2a622d` |
| 88 | `ISCompositeLensFlare` | 3 | `4913b38bb221bcfd3048a02e0436e3b745a2c8cbdb0efb1042241065c43f5fdf` |
| 89 | `ISCompositeLensFlareVolumetricLighting` | 8 | `8c00d4e73844d2597b3e4a25d1970e722481cd9e1168e9e57f0c8fac13fa2b8c` |
| 92 | `ISDownsample` | 27 | `655a98d7df9ad47b1df75dfd7b4f930a4e04624d25e5982c9e71a9e862925fe9` |
| 93 | `ISDownsampleIgnoreBrightest` | 17 | `14a6762bfcb4931543a42b088cbd305103e08256f5aa84aec2b7b4380d7fddde` |
| 96 | `ISExp` | 5 | `93c39d3f214b66a04b2cded257f5fff6bbcb9cd0a1f8246cfd9bb04e00d3d9e3` |
| 97 | `ISIBLensFlares` | 40 | `62c06518e41dcc8582c2390ac65bfa99c729d5bad6eef502c3e42b07cbabd53f` |
| 98 | `ISLightingComposite` | 23 | `2eabe9816da65e93c6fbd478a4fa863987c26b686831cc845fa017e72a04c444` |
| 99 | `ISLightingCompositeNoDirectionalLight` | 18 | `b548150d5a8b92b4c977145c226cb0dede84b88dea8a0b2a5e61144c88822692` |
| 100 | `ISLightingCompositeMenu` | 19 | `a8068334e0401bae0f31260626c1a2815f423e438f388bd1dda03aa21bee4115` |
| 111 | `ISSAOCompositeSAO` | 184 | `5549c537487720a4ce521fec3595a78a197c39586cb7720b69605718a043f31a` |
| 112 | `ISSAOCompositeFog` | 196 | `d4b6f615b16fbcf0015716c373b75608b9bf3c8ba2af864d095655daa82ca799` |
| 113 | `ISSAOCompositeSAOFog` | 203 | `df277f776cd0c130f00e8b52ca55b4d58611b631f16a04cf12f8e80c74663aeb` |
| 119 | `ISSILComposite` | 4 | `3a1a7a6ecbee15d806ba85d9173dfb750b5f313b21eddb3e1119ff08b1b64117` |
| 123 | `ISSnowSSS` | 45 | `733544a5ced52d85401f50c8fbfadb86784054bce38516fb333ccace93e2816a` |
