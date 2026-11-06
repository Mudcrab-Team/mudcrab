# Embedded executable shader catalog

The bounded executable scan found 57 structurally valid DXBC occurrences containing 53 distinct bytecodes. All 53 were extracted from their exact file offsets and freshly decoded. There are 12 vertex-shader and 45 pixel-shader occurrences: 56 declare Shader Model 4.0 and one declares Shader Model 5.0.

These programs retain `RDEF` reflection metadata. They are cataloged separately from the 16,044 programs in `shaders011.fxp`, which retain no `RDEF` chunks. [embedded-shaders.json](embedded-shaders.json) records every candidate occurrence, bytecode hash, signature, reflected name, binding range, constant-buffer variable offset and type, surviving declaration and instruction footprint.

| Identity | Value |
| --- | --- |
| Executable SHA-256 | `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f` |
| Embedded catalog SHA-256 | `36ccfd96ef318f5652fd1f8415e325a82614cbb6d965ea8e98aa7c8e1d157198` |
| Package bytecodes with identical complete embedded bytes | 0 distinct embedded programs |
| Package programs with identical shader instruction-token payloads | 0 distinct embedded programs |

Every candidate's bytes and chunks match the owning executable scan. Every reflected string, type, member and variable range was bounded against its original reflection chunk. All declared CB, SRV and sampler slots have a matching reflected binding range. Independent token walking consumed every instruction stream and matched decoded executable and declaration counts. The executable and input scan file remained unchanged.

The source retains original resource names including `VSConstants`, `PSConstants`, `tex`, `srctex`, `tex_y`, `tex_u`, `tex_v`, `tex_a`, `SampBase` and `TexBase`. Those names identify interfaces to trace. They do not establish the native producer, current resource contents or the role of a pass. Compiler-creator strings and numeric compilation flags are recorded as metadata, without inferring game settings.

## Every candidate occurrence

The table shows original reflected binding names and register classes. Matching names across programs do not establish shared resource instances. Byte-identical occurrences remain separate rows because native references can differ.

| Embedded VA | Stage / model | Bytes | Executable instructions | Original reflected bindings |
| --- | --- | ---: | ---: | --- |
| `0x141a9e2f0` | VS / 4.0 | 884 | 7 | `VSConstants` (b0) |
| `0x141a9e670` | VS / 4.0 | 840 | 6 | `VSConstants` (b0) |
| `0x141a9e9c0` | VS / 4.0 | 784 | 6 | `VSConstants` (b0) |
| `0x141a9ecd0` | VS / 4.0 | 860 | 8 | `VSConstants` (b0) |
| `0x141a9f030` | VS / 4.0 | 960 | 9 | `VSConstants` (b0) |
| `0x141a9f3f0` | VS / 4.0 | 1052 | 10 | `VSConstants` (b0) |
| `0x141a9f810` | VS / 4.0 | 1152 | 12 | `VSConstants` (b0) |
| `0x141a9fc90` | VS / 4.0 | 816 | 7 | `VSConstants` (b0) |
| `0x141a9ffc0` | VS / 4.0 | 916 | 8 | `VSConstants` (b0) |
| `0x141aa0360` | VS / 4.0 | 1008 | 9 | `VSConstants` (b0) |
| `0x141aa0750` | VS / 4.0 | 1108 | 11 | `VSConstants` (b0) |
| `0x141aa0bb0` | PS / 4.0 | 1020 | 6 | `samp` (s0), `tex0` (t0), `tex1` (t1), `PSConstants` (b0) |
| `0x141aa0fb0` | PS / 4.0 | 704 | 4 | `PSConstants` (b0) |
| `0x141aa1270` | PS / 4.0 | 772 | 5 | `PSConstants` (b0) |
| `0x141aa1580` | PS / 4.0 | 708 | 4 | `PSConstants` (b0) |
| `0x141aa1850` | PS / 4.0 | 1024 | 8 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aa1c50` | PS / 4.0 | 612 | 2 | `PSConstants` (b0) |
| `0x141aa1ec0` | PS / 4.0 | 956 | 7 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aa2280` | PS / 4.0 | 1108 | 8 | `samp` (s0), `tex0` (t0), `tex1` (t1), `PSConstants` (b0) |
| `0x141aa26e0` | PS / 4.0 | 764 | 3 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aa29e0` | PS / 4.0 | 852 | 5 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aa2d40` | PS / 4.0 | 1124 | 14 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aa31b0` | PS / 4.0 | 1212 | 16 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aa3670` | PS / 4.0 | 1312 | 24 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aa3b90` | PS / 4.0 | 1400 | 26 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aa4110` | PS / 4.0 | 1740 | 29 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa47e0` | PS / 4.0 | 2040 | 36 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa4fe0` | PS / 4.0 | 2040 | 36 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa57e0` | PS / 4.0 | 1708 | 28 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa5e90` | PS / 4.0 | 1828 | 31 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa65c0` | PS / 4.0 | 2128 | 38 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa6e10` | PS / 4.0 | 2128 | 38 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa7660` | PS / 4.0 | 1796 | 30 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa7d70` | PS / 4.0 | 1728 | 29 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa8430` | PS / 4.0 | 1896 | 33 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa8ba0` | PS / 4.0 | 1924 | 34 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa9330` | PS / 4.0 | 1756 | 30 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aa9a10` | PS / 4.0 | 1816 | 31 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aaa130` | PS / 4.0 | 1984 | 35 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aaa8f0` | PS / 4.0 | 2012 | 36 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aab0d0` | PS / 4.0 | 1844 | 32 | `srctex_s` (s0), `tex_s` (s1), `srctex` (t0), `tex` (t1), `PSConstants` (b0) |
| `0x141aab810` | PS / 4.0 | 1416 | 25 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aabda0` | PS / 4.0 | 1416 | 25 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aac330` | PS / 4.0 | 1504 | 27 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aac910` | PS / 4.0 | 1504 | 27 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aacef0` | PS / 4.0 | 940 | 8 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aad2a0` | PS / 4.0 | 1028 | 10 | `tex_s` (s0), `tex` (t0), `PSConstants` (b0) |
| `0x141aad6b0` | PS / 4.0 | 520 | 2 | `PSConstants` (b0) |
| `0x141aad8c0` | PS / 4.0 | 852 | 5 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aadc20` | PS / 4.0 | 792 | 3 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aadf40` | PS / 4.0 | 880 | 5 | `samp` (s0), `tex0` (t0), `PSConstants` (b0) |
| `0x141aae2b0` | PS / 4.0 | 1396 | 15 | `samp_y` (s0), `samp_u` (s1), `samp_v` (s2), `tex_y` (t0), `tex_u` (t1), `tex_v` (t2), `PSConstants` (b0) |
| `0x141aae830` | PS / 4.0 | 1516 | 15 | `samp_y` (s0), `samp_u` (s1), `samp_v` (s2), `samp_a` (s3), `tex_y` (t0), `tex_u` (t1), `tex_v` (t2), `tex_a` (t3), `PSConstants` (b0) |
| `0x141aaee20` | PS / 4.0 | 1604 | 17 | `samp_y` (s0), `samp_u` (s1), `samp_v` (s2), `samp_a` (s3), `tex_y` (t0), `tex_u` (t1), `tex_v` (t2), `tex_a` (t3), `PSConstants` (b0) |
| `0x141aaf470` | PS / 4.0 | 1484 | 17 | `samp_y` (s0), `samp_u` (s1), `samp_v` (s2), `tex_y` (t0), `tex_u` (t1), `tex_v` (t2), `PSConstants` (b0) |
| `0x1420d3770` | VS / 4.0 | 568 | 4 | — |
| `0x1420d39b0` | PS / 5.0 | 696 | 3 | `SampBase` (s0), `TexBase` (t0) |

## Open links

Two occurrences have verified native construction links. Function `0x1411B2990` passes the 568-byte VS at `0x1420D3770` to creator `0x14100FDD0` at call site `0x1411B306F`, then the 696-byte PS at `0x1420D39B0` to creator `0x14100FF30` at `0x1411B3099`. The corresponding returned objects are stored at offsets `+0x58` and `+0x60`. All 610 original instruction records used from the containing function and both creators were checked against the executable bytes. Query evidence hashes and individual load/call sites are recorded in the catalog.

The remaining 55 occurrences need a native reference chain into shader creation and selection. Construction establishes that native code can construct a shader; it does not establish a draw, dispatch, active scene condition, resource identity or pass order. Semantic pass roles remain unassigned for all 57 occurrences.

Reflection names are original metadata. Constant-buffer variable names, byte offsets, row/column packing and array sizes are recorded exactly; producer calculations remain open. Default-value regions are bounded and hashed, with their contents kept private. Unknown type-extension words retain their numeric values without a semantic label.

The 57 occurrences are the validated result of this scan. Dynamically generated, vendor, fallback, loose or externally loaded shader code requires separate discovery. No retail Windows frame capture or GPU execution was performed. Original shader bytes and assembly remain private.
