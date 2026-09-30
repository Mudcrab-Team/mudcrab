# OpenSkyrim Bevy Engine Examples & Patterns

This document maps official **Bevy 0.19 Examples** (`bevy.org/examples`) directly into OpenSkyrim subsystem implementations.

---

## 1. Subsystem to Bevy Example Mapping

| OpenSkyrim Feature                    | Bevy Example Category   | Target Bevy Example               | OpenSkyrim Adaptation                                                                                          |
| :------------------------------------ | :---------------------- | :-------------------------------- | :------------------------------------------------------------------------------------------------------------- |
| **Cell Spatial Streaming**            | `Async Tasks` / `Scene` | `async_channel`, `scene`          | Background thread streams SQLite bounding box objects & spawns `bsn!` scene bundles without UI stutter.        |
| **PBR & Environment Lighting**        | `3D Rendering`          | `pbr`, `lighting`, `atmosphere`   | Skyrim sun direction, volumetric fog, and PBR metallic roughness materials.                                    |
| **Instanced Foliage & Trees**         | `3D Rendering`          | `instancing`, `mesh_custom`       | Vercidium-style GPU indirect draw calls for rendering millions of trees and grass blades.                      |
| **Character Animations**              | `Animation`             | `animated_fox`, `animation_graph` | Skeletal animation playback for NPC walking, running, and sword attacks mapped from converted glTF animations. |
| **3D Spatial Audio & Weather Sounds** | `Audio`                 | `spatial_audio_3d`                | Positional 3D audio emitting dungeon echoes, river streams, and dragon roars relative to camera position.      |
| **Native UI & HUD**                   | `UI`                    | `ui`, `bsn_ui`, `borders`         | Health bars, compass, dialogue selection, and inventory screens rendered with Bevy 0.19 `bsn!` macros.         |

---

## 2. Code Snippet Adaptations

### A. 3D Spatial Audio (`spatial_audio_3d.rs` ➔ Dungeon Echoes)

```rust
use bevy::audio::{AudioPlugin, SpatialAudioSink};

fn spawn_dungeon_river_sound(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
) {
    commands.spawn((
        AudioPlayer::new(asset_server.load("sounds/river_stream.ogg")),
        PlaybackSettings::LOOP.with_spatial(true),
        Transform::from_xyz(12.0, 0.0, -5.0), // Sound emitted from river coordinate
    ));
}
```

### B. Instanced Foliage Rendering (`instancing.rs` ➔ Skyrim Forests)

```rust
// Groups thousands of PineTree glTF instances into a single GPU draw call
fn setup_instanced_forest(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let tree_mesh = meshes.add(Cuboid::default()); // Replace with glTF PineTree handle
    let tree_material = materials.add(Color::srgb(0.1, 0.4, 0.1));

    // Spawn 5,000 instanced tree transforms in 1 GPU draw call
    commands.spawn_batch((0..5000).map(|i| {
        (
            Mesh3d(tree_mesh.clone()),
            MeshMaterial3d(tree_material.clone()),
            Transform::from_xyz((i % 70) as f32 * 4.0, 0.0, (i / 70) as f32 * 4.0),
        )
    }));
}
```

---

## 3. Benefits of Leveraging Official Bevy Examples

1. **API Compliance:** Using official Bevy 0.19 patterns guarantees compatibility with upcoming engine releases.
2. **Built-in Performance:** Bevy's official examples are written by core engine maintainers, ensuring optimal memory allocations and GPU pipeline usage.
