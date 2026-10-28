// SPDX-License-Identifier: MIT AND GPL-3.0-only
//
// Model-space normal (`_msn`) decode, world-normal transform, FaceGen base colour, skin RGB
// tint and hair tint of Skyrim's lighting shader.
//
// Ported from Community Shaders (https://github.com/community-shaders/skyrim-community-shaders,
// commit e5ae2d0082469f92ac08dda89d230dc2d9328ba0), package/Shaders/Lighting.hlsl:
//   - lines 453-456: on model-space materials (not LOD land noise) `_s` is its own texture, t2;
//   - lines 1591-1594, PS `MODELSPACENORMALS`, plain branch: `normal.xyz = normal.xzy * 2 - 1`,
//     `glossiness = TexSpecularSampler.Sample(...).x`;
//   - line 1725, PS: `worldNormal = normalize(mul(tbn, normal.xyz))`;
//   - lines 2001-2003, PS: `material.Glossiness = HasSpecular() ? glossiness : 0`;
//   - lines 684-694, `GetFacegenBaseColor` (FaceGen tint overlay and detail map), and its call
//     sites at lines 1492-1497 (linear lighting: applied to the raw diffuse sample, then
//     `DiffuseToWorking`) and 1623-1624 (gamma lighting);
//   - lines 696-703, `GetFacegenRGBTintBaseColor` (the same overlay with the constant
//     `TintColor`, the skin tone of body, hands and feet), and its call sites at lines 1494-1495
//     (linear lighting) and 1625-1626 (gamma lighting);
//   - line 2709, PS `#if defined(HAIR)`: the hair vertex colour
//     `SRGBToWorking(lerp(1, TintColor.xyz, input.Color.y))`, which line 2821 multiplies into the
//     diffuse and ambient light (`color.xyz *= vertexColor`) before specular is added; and
//     lines 270-274, VS: `vsout.Color = 1.0.xxxx` for a shape without vertex colours.
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
//   - `GetFacegenBaseColor` takes its two textures and samplers as parameters instead of the
//     global `TexDetailSampler` / `TexTintSampler` registers; the math is unchanged.
//     `GetFacegenRGBTintBaseColor` takes `TintColor` as a parameter instead of reading the
//     `PerMaterial` constant buffer, and drops its unused `uv`; the math is unchanged. Its caller
//     handles the colour space the same way as for `GetFacegenBaseColor`. Its caller
//     (`skyrim_skin.wgsl`) feeds it gamma-encoded values, as CS does: the tint and detail maps
//     are sampled without sRGB decoding, and Bevy's decoded base colour is re-encoded with the
//     sRGB curve first. The result goes back through the sRGB curve, where CS's linear-lighting
//     path uses `DiffuseToWorking` with its configurable diffuse gamma and curve.
//   - The hair vertex colour is split in two: `get_hair_vertex_color` is the `lerp`, and its
//     caller (`skyrim_skin.wgsl`) applies `SRGBToWorking` as the sRGB decode, where CS's linear
//     lighting path can also convert to another working space. The caller multiplies the result
//     into the base colour instead of the lit colour: Bevy's diffuse and ambient terms scale with
//     the base colour and its dielectric specular does not, which is the same split line 2821
//     makes. CS's own hair shading (`CS_HAIR_SHADING`, which sets the vertex colour to 1) is not
//     ported. On hair the vertex colour's red and blue channels are not colour, so the caller
//     drops them from Bevy's base colour and keeps alpha, as `alpha *= input.Color.w` (line 2998)
//     does.
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

// Lighting.hlsl, `#if defined(FACEGEN)`:
//     float3 GetFacegenBaseColor(float3 rawBaseColor, float2 uv)
//     {
//         float3 detailColor = TexDetailSampler.Sample(SampDetailSampler, uv).xyz;
//         detailColor = float3(3.984375, 3.984375, 3.984375) * (float3(0.00392156886, 0, 0.00392156886) + detailColor);
//         float3 tintColor = TexTintSampler.Sample(SampTintSampler, uv).xyz;
//         tintColor = tintColor * rawBaseColor * 2.0.xxx;
//         tintColor = tintColor - tintColor * rawBaseColor;
//         return (rawBaseColor * rawBaseColor + tintColor) * detailColor;
//     }
// The detail map is centred near 64/255 (x4 brings it to about 1), and the tint is an overlay:
// `base^2 + 2 * tint * base * (1 - base)`, which leaves the base unchanged at tint 0.5.
fn get_facegen_base_color(
    raw_base_color: vec3<f32>,
    uv: vec2<f32>,
    tex_tint: texture_2d<f32>,
    samp_tint: sampler,
    tex_detail: texture_2d<f32>,
    samp_detail: sampler,
) -> vec3<f32> {
    var detail_color = textureSample(tex_detail, samp_detail, uv).xyz;
    detail_color = vec3<f32>(3.984375, 3.984375, 3.984375)
        * (vec3<f32>(0.00392156886, 0.0, 0.00392156886) + detail_color);
    var tint_color = textureSample(tex_tint, samp_tint, uv).xyz;
    tint_color = tint_color * raw_base_color * vec3<f32>(2.0);
    tint_color = tint_color - tint_color * raw_base_color;
    return (raw_base_color * raw_base_color + tint_color) * detail_color;
}

// Lighting.hlsl, `#if defined(FACEGEN_RGB_TINT)`:
//     float3 GetFacegenRGBTintBaseColor(float3 rawBaseColor, float2 uv)
//     {
//         float3 tintColor = TintColor.xyz * rawBaseColor * 2.0.xxx;
//         tintColor = tintColor - tintColor * rawBaseColor;
//         return float3(1.01171875, 0.99609375, 1.01171875) * (rawBaseColor * rawBaseColor + tintColor);
//     }
// The FaceGen overlay with a constant tint: `base^2 + 2 * tint * base * (1 - base)`, unchanged at
// tint 0.5 up to the small per-channel factor.
fn get_facegen_rgb_tint_base_color(raw_base_color: vec3<f32>, tint: vec3<f32>) -> vec3<f32> {
    var tint_color = tint * raw_base_color * vec3<f32>(2.0);
    tint_color = tint_color - tint_color * raw_base_color;
    return vec3<f32>(1.01171875, 0.99609375, 1.01171875)
        * (raw_base_color * raw_base_color + tint_color);
}

// Lighting.hlsl, PS, `#if defined(HAIR)`:
//     float3 vertexColor = ColorManagement::SRGBToWorking(lerp(1, TintColor.xyz, input.Color.y));
// The vertex colour's green channel masks the hair tint; the result is still encoded, and the
// caller decodes it (`SRGBToWorking`).
fn get_hair_vertex_color(tint_color: vec3<f32>, vertex_color_y: f32) -> vec3<f32> {
    return mix(vec3<f32>(1.0), tint_color, vertex_color_y);
}
