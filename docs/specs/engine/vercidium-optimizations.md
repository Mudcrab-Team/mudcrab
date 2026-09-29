# OpenSkyrim High-Performance Rendering & Engine Optimizations

Inspired by key engine optimizations demonstrated by **Vercidium** (developer of the custom high-FPS engine _Sector's Edge_).

---

## 1. Zero State-Change GPU Instancing (`DrawMeshInstancedIndirect`)

### The Problem in Creation Engine:

In original Skyrim, every tree, rock, building, or barrel incurs an individual CPU-to-GPU draw call. Rendering Whiterun or a dense forest creates thousands of driver state changes per frame, overwhelming the CPU main thread.

### The OpenSkyrim Solution:

- **GPU Instancing Batching:**
  - Group identical glTF models (e.g. `PineTree01.glb`, `WhiterunWall02.glb`).
  - Send **a single draw call** to the GPU along with an instance buffer containing array transforms (`Matrix4x4` positions, rotations, scales) and material variation flags.
- **Indirect Drawing:**
  - Use WebGPU / Vulkan `draw_indexed_indirect`. The GPU reads the draw parameters straight out of GPU memory without waiting for CPU instructions.

---

## 2. Spatial Grid Partitioning & Frustum Culling

### The Vercidium Concept:

Divide the game world into uniform spatial chunks/grid cells and eliminate non-visible geometry before it touches the render pipeline.

### OpenSkyrim Implementation:

1. **Bounding Volume Hierarchy (BVH) / Spatial Chunks:**
   - Exterior cells in Skyrim are divided into 32x32 world grids.
   - Calculate Axis-Aligned Bounding Boxes (AABB) for all static world objects during the offline pipeline transformation.
2. **GPU / Job System Frustum Culling:**
   - Before sending geometry to the rasterizer, test bounding boxes against the 6 camera view frustum planes in parallel using Bevy's rayon work-stealing threadpool.
   - Instantly drop 80%+ of world geometry that sits behind or outside the player's FOV.

---

## 3. Hardware Occlusion & Hierarchical Z-Buffer (HZB) Culling

### The Skyrim "Overdraw" Problem:

Skyrim often renders objects obscured behind mountains, city walls, or closed doors because CPU occlusion checking is primitive.

### OpenSkyrim Solution:

- **Hierarchical Z-Buffer (HZB) Culling:**
  - Generate a low-resolution depth pyramid map from the previous frame's depth buffer.
  - Check the 3D bounding box of complex objects (e.g., dungeons or buildings inside Solitude) against the depth pyramid.
  - If a mountain occludes an entire town, **skip rendering the town completely**.

---

## 4. Persistent Memory Mapping & Zero-Allocation Loops

### Vercidium Technique:

Avoid Garbage Collection (GC) pressure and heap allocations during frame rendering loops.

### OpenSkyrim (Rust Native Advantage):

- **Zero Heap Allocations at 60+ FPS:**
  - All ECS system queries in Bevy reuse pre-allocated array buffers (`Vec::clear()` instead of re-allocating).
- **`mmap` Persistent Buffers:**
  - Map converted `.glb` geometry and `SQLite3` index pages directly into virtual memory. The CPU never copies asset buffers across heap boundaries.

---

## 5. Summary Table of Optimization Techniques

| Optimization Technique | Original Skyrim Behavior                        | **OpenSkyrim Vercidium-Style Engine**                   |
| :--------------------- | :---------------------------------------------- | :------------------------------------------------------ |
| **Draw Calls**         | 1 draw call per static object (Thousands/frame) | **Batched Instanced Indirect Draw Calls** (< 100/frame) |
| **Memory Allocation**  | Runtime allocation during cell loads            | **Pre-allocated buffers + `mmap` zero-copy**            |
| **Culling**            | Basic distance & cell culling                   | **Frustum Culling + GPU HZB Occlusion Culling**         |
| **Script Threading**   | Single-threaded VM (Papyrus blockages)          | **Parallel Luau Fiber Coroutines**                      |
