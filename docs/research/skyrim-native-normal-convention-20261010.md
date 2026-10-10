# Farmhouse normal-frame audit, 2026-10-10

The checked path demonstrates no double or missing normal-Y flip, reordered
source normals or doubled Creation basis conversion. It does expose a remaining
frame difference: Mudcrab regenerates tangents and reconstructs bitangents,
where Skyrim's ordinary vertex shader consumes authored T/B/N independently.
The audit does not measure that difference's contribution to the glowing frame.

Stone `Farmhouse01:1` and roofs `:4`/`:25` have 444, 468 and 382 vertices.
Their exported NORMAL accessors match the retained source metadata in binary32,
with zero mismatches. Material names are distinct from glTF material indices.
All three use linear BC3_UNORM normal containers, omit a TANGENT accessor and
declare `generated_mikktspace`, `sampledNormalYCorrection=true` and
`authoredFrameEvaluated=false`.

The Creation-to-runtime scene-root rotation maps `(x,y,z)` to `(x,z,-y)` and
preserves handedness. Vertex data remains Creation-local. Bevy generates missing
tangents and negates tangent.w. The native fragment shader applies one sampled-Y
correction, reconstructs B as `tangent.w*cross(N,T)`, then normalizes the mapped
normal. StandardMaterial's Y-flip flag belongs to the fallback path and is not
read by the active native shader. These two fields therefore do not create two
sampled-Y flips in the native path.

The inspected ordinary Skyrim shaders decode full RGB as `2*c-1`, interpolate
authored frame vectors independently, combine them and normalize the result.
They supply no unconditional green inversion. Normalizing regenerated vertex
N/T and reconstructing B can differ from those quantized authored vectors.
Retained source-frame metadata establishes availability, not runtime evaluation.

No global flip is justified by this evidence. An authored-frame comparison first
needs the original input layout and component decoding, then separate authored,
generated and geometric-normal controls on the exact surfaces. Original DDS
normal-payload equality was not independently revalidated in this audit. No
build, GPU execution, texture reencoding or new native query was performed.

The [JSON evidence](skyrim-native-normal-convention-20261010.json) records source
references, hashes, sampler metadata and checked primitive counts.
