// SPDX-License-Identifier: MIT AND GPL-3.0-only
//
// Model-space normal (`_msn`) decode and world-normal transform of Skyrim's lighting shader.
//
// Ported from Community Shaders (https://github.com/community-shaders/skyrim-community-shaders,
// commit e5ae2d0082469f92ac08dda89d230dc2d9328ba0), package/Shaders/Lighting.hlsl:
//   - lines 453-456: on model-space materials (not LOD land noise) `_s` is its own texture, t2;
//   - lines 1591-1594, PS `MODELSPACENORMALS`, plain branch: `normal.xyz = normal.xzy * 2 - 1`,
//     `glossiness = TexSpecularSampler.Sample(...).x`;
//   - line 1725, PS: `worldNormal = normalize(mul(tbn, normal.xyz))`;
//   - lines 2001-2003, PS: `material.Glossiness = HasSpecular() ? glossiness : 0`.
// Lighting.hlsl's own header: original code MIT (Ilya Perapechka), modifications GPL-3.0 with
// Community Shaders' exceptions.
//
// Copyright (c) 2022 Ilya Perapechka (original shader code, MIT; notice below)
// Copyright (C) the Community Shaders contributors (modifications, GPL-3.0-only)
//
// Community Shaders licenses its modifications under GPL-3.0 with a Modding Exception and a
// GPL-3.0 linking exception (its EXCEPTIONS.md). This file drops both exceptions, as EXCEPTIONS.md
// allows, and is GPL-3.0-only for the modifications; the original code keeps its MIT licence.
//
// What changed in the port:
//   - HLSL to WGSL. HLSL builds `tbn` from rows (`float3x3(TBN0, TBN1, TBN2)`) and multiplies
//     with `mul(tbn, n)`; here the matrix is built from columns and multiplied as `tbn * n`, which
//     is the same product.
//   - The VS TBN (`#elif defined(SKINNED)`, the rows of `boneRSMatrix` with each column
//     normalised) is not ported. The TBN here is the inverse transpose of the skinned (or mesh)
//     world-from-local 3x3, which Bevy's vertex stage already computes for its own normals. The
//     two agree after the final normalize for rotation with uniform scale, which is all Skyrim's
//     bones carry; the inverse transpose stays correct under non-uniform scale.
//   - Non-skinned model-space shapes go through the mesh's world-from-local as well. Vanilla
//     lights them in model space instead, which is the same result.
//   - Only the functions the Mudcrab skin material calls are ported. Lighting itself is Bevy's
//     PBR model, not Skyrim's.
//
// MIT License
//
// Copyright (c) 2022 Ilya Perapechka
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#define_import_path mudcrab::cs_lighting_msn

// Lighting.hlsl, PS, `#if defined(MODELSPACENORMALS)`, the plain (not LOD land) branch:
//     normal.xyz = normal.xzy * 2.0.xxx + -1.0.xxx;
//     normal.w = 1;
// The map stores the shape-space normal with its Y and Z channels swapped.
fn decode_model_space_normal(sampled: vec4<f32>) -> vec4<f32> {
    var normal = sampled;
    normal = vec4<f32>(normal.xzy * vec3<f32>(2.0) + vec3<f32>(-1.0), normal.w);
    normal.w = 1.0;
    return normal;
}

// Lighting.hlsl, PS, same branch:
//     glossiness = TexSpecularSampler.Sample(SampSpecularSampler, uv).x;
// On model-space materials the `_s` map holds gloss; the normal map's alpha does not.
fn model_space_glossiness(specular_sample: vec4<f32>) -> f32 {
    return specular_sample.x;
}

// Lighting.hlsl, PS:
//     float3 worldNormal = normalize(mul(tbn, normal.xyz));
fn model_space_world_normal(tbn: mat3x3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let world_normal = normalize(tbn * normal);
    return world_normal;
}

// Lighting.hlsl, PS:
//     const bool hasSpecular = HasSpecular();
//     material.Glossiness = hasSpecular ? glossiness : 0;
fn material_glossiness(has_specular: bool, glossiness: f32) -> f32 {
    return select(0.0, glossiness, has_specular);
}
