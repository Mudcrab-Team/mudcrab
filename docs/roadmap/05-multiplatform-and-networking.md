# Phase 5: Hardware Ray-Tracing, Multiplatform & Networking

> **Goal:** Extend OpenSkyrim with modern rendering pipelines, cross-platform compilation targets, VR support, and native co-op multiplayer.

---

## 🎯 Key Deliverables & Specifications

### 5.1 Hardware Ray-Tracing & Upscaling Pipeline

- Ray-traced global illumination (RTGI), ambient occlusion, soft shadows, and dynamic reflections.
- Upscaling framework support (DLSS 3, FSR 3, XeSS frame generation) via `wgpu`.

### 5.2 Cross-Platform Target Matrix

- **Android ARM64 APK:** Optimized VRAM footprint for mobile devices (POCO F5 / Snapdragon 7+ Gen 2+).
- **Linux & macOS:** Native Vulkan / Metal support (optimized for Steam Deck & Apple Silicon).
- **WebAssembly:** In-browser WebGPU runtime client.

### 5.3 Native Co-op Multiplayer Engine

- Native Bevy ECS entity replication over WebSockets / UDP.
- Synchronization of player transforms, animations, combat actions, and world entity deltas.

### 5.4 Advanced Generative AI & VR Extensions

- Optional Luau bindings to local LLMs (e.g. **Ollama**) for dynamic, unscripted NPC dialogue.
- Native OpenXR stereo rendering and motion controller bindings for PCVR.
