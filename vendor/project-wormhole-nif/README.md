# Local NIF parser and exporter patches

This directory vendors `project-wormhole-nif` 0.1.0, licensed
GPL-3.0-or-later. It is selected through the workspace path dependency.

The authored geometry changes extend the existing local parser and exporter:

- Retain authored node hierarchy, transforms, vertex attributes, material slots,
  skin bindings and ordinary model placement during glTF export.
- Record source-block identity when glTF nodes are emitted. The converter uses
  this map to resolve controller object-palette targets without guessing among
  duplicate node names.
- Parse `NiBillboardNode` once and preserve its mode in node extras. In
  `populate_static_scene`, billboard nodes use the dedicated arm that records
  `billboard_mode`, rather than the generic node-subtype arm.

The converter regression `billboard_metadata_survives_nif_to_glb_export` in
`crates/converter/tests/animation_notices.rs` writes a dummy-content NIF and
checks the actual exported GLB node extras and mesh hierarchy. The same test
file covers duplicate-name palette targeting through the public exporter.
