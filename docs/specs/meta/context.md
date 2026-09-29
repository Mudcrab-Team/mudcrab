# OpenSkyrim Domain Context

## Glossary & Canonical Terms

### 1. Offline Transpilation Pipeline (`pipeline`)

The ahead-of-time batch conversion system that ingests original Bethesda assets (`.esm`, `.esp`, `.bsa`, `.nif`, `.dds`, `.pex`) and compiles them into modern standard runtime assets (`SQLite 3`, `rkyv`, `glTF 2.0`, `KTX2`, `Luau`).

### 2. Unified Master Database (`skyrim_world.db`)

A single SQLite 3 database created by compiling `Skyrim.esm` and all active plugin overrides (`.esp`/`.esl`) in priority order determined by `plugins.txt`. Records with matching `FormID` are pre-merged so runtime reads are `O(1)` without requiring runtime plugin conflict resolution.

### 3. Spatial R-Tree Index (`refr_spatial`)

An SQLite 3 virtual R-Tree table indexing placed world references (`REFR`) by 3D axis-aligned bounding boxes (`minX, maxX, minY, maxY, minZ, maxZ`). Used for ultra-fast sub-millisecond cell camera visibility queries.

### 4. Zero-Copy Terrain Cache (`cell_cache.rkyv`)

Memory-mapped (`mmap`) binary buffers using `rkyv` zero-copy serialization for high-density spatial data (such as 33x33 heightmap grids and vertex normals from `LAND` records) to allow 0-CPU-cost reads directly from RAM.
