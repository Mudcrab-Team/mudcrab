# meshoptimizer 1.3

This directory contains the source subset needed by `meshopt-cluster`.

## Provenance and licensing

- Source: https://github.com/zeux/meshoptimizer, tag `v1.3`.
- Upstream commit: `9e1f07b159d3cb777f1c67ed31fc11fd117986f4`.
- Archive: https://codeload.github.com/zeux/meshoptimizer/tar.gz/refs/tags/v1.3
- Archive SHA-256: `ca4f47d451201593904b9e3067fa54b65db94bc34ab5173864950862b748a145`.
- Included files: `LICENSE.md`, all 22 files from upstream `src/`, and
  `demo/clusterlod.h`. Other upstream examples, tests and build files are omitted.
- Compiled files: `crates/meshopt-cluster/build.rs` builds only
  `clusterizer.cpp`, `indexgenerator.cpp`, `meshletutils.cpp`, `partition.cpp`,
  `simplifier.cpp` and `spatialorder.cpp`, the sources `clusterlod.h` links
  against. The other `src/` files stay in the tree unmodified for comparison
  with the archive but are not built. When `clusterlod.h` or the wrapper starts
  calling another meshoptimizer function, the linker reports the missing symbol;
  add the source that defines it to `VENDOR_SOURCES` in `build.rs`.
- Licence: MIT; `LICENSE.md` and the clusterlod copyright/licence notice retain
  their upstream bytes.

## Local changes

Only `demo/clusterlod.h` differs from the upstream source subset:

- Include `<limits>` to obtain the unsigned protect-mask width.
- Short-circuit the attribute protect-mask bound assertion at that width,
  avoiding an undefined shift while preserving all 32 attribute bits.
- Limit seam-bit inspection to that width; larger strides are padding beyond
  the protect mask and must not shift by 32 or more.

The Rust wrapper validates indices, attributes, locks and configuration before
calling native code. Empty triangle lists return an empty hierarchy after those
checks, avoiding clusterlod's empty position-remap access. Native assertions
remain enabled in debug builds; disabling them is not a substitute for bounds
validation. On upgrades, compare every included file against the tagged archive,
retain the licence notices and re-evaluate the local patch and wrapper limits.
