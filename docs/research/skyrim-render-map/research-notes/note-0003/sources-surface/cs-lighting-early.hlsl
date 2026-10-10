#if (defined(TREE_ANIM) || defined(LANDSCAPE)) && !defined(VC)
#define VC
#endif

#if defined(SPECULAR) || defined(AMBIENT_SPECULAR) || defined(ENVMAP) || defined(RIM_LIGHTING) || defined(PARALLAX) || defined(MULTI_LAYER_PARALLAX) || defined(FACEGEN) || defined(FACEGEN_RGB_TINT) || defined(SNOW_FLAG) || defined(EYE) || defined(PBR)
#define HAS_VIEW_VECTOR
#endif

struct VS_INPUT
{
    float4 Position									: POSITION0;
    float2 TexCoord0								: TEXCOORD0;
#if !defined(MODELSPACENORMALS)
    float4 Normal									: NORMAL0;
    float4 Bitangent								: BINORMAL0;
#endif
	
#if defined(VC)
	float4 Color									: COLOR0;
#if defined(LANDSCAPE)
	float4 LandBlendWeights1						: TEXCOORD2;
	float4 LandBlendWeights2						: TEXCOORD3;
#endif
#endif
#if defined(SKINNED)
	float4 BoneWeights								: BLENDWEIGHT0;
	float4 BoneIndices								: BLENDINDICES0;
#endif
#if defined(EYE)
	float EyeParameter								: TEXCOORD2;
#endif
};

struct VS_OUTPUT
{
    float4 Position									: SV_POSITION0;
#if (defined (PROJECTED_UV) && !defined(SKINNED)) || defined(LANDSCAPE)
	float4
#else
    float2
#endif
	TexCoord0										: TEXCOORD0;
#if defined (ENVMAP) 
	precise
#endif
    float3 InputPosition							: TEXCOORD4;
#if defined(SKINNED) || !defined(MODELSPACENORMALS)
    float3 TBN0										: TEXCOORD1;
    float3 TBN1										: TEXCOORD2;
    float3 TBN2										: TEXCOORD3;
#endif
#if defined(HAS_VIEW_VECTOR)
    float3 ViewVector								: TEXCOORD5;
#endif
#if defined(EYE)
	float3 EyeNormal								: TEXCOORD6;
#elif defined(LANDSCAPE)
	float4 LandBlendWeights1						: TEXCOORD6;
	float4 LandBlendWeights2						: TEXCOORD7;
#elif defined(PROJECTED_UV) && !defined(SKINNED)
	float3 TexProj									: TEXCOORD7;
#endif
    float3 ScreenNormalTransform0					: TEXCOORD8;
    float3 ScreenNormalTransform1					: TEXCOORD9;
    float3 ScreenNormalTransform2					: TEXCOORD10;
    float4 WorldPosition							: POSITION1;
    float4 PreviousWorldPosition					: POSITION2;
    float4 Color									: COLOR0;
    float4 FogParam									: COLOR1;
};

#ifdef VSHADER
cbuffer PerFrame									: register(b12)
{
	row_major float3x3 ScreenProj					: packoffset(c0);
	row_major float4x4 ViewProj						: packoffset(c8);
#if defined (SKINNED)
	float3 BonesPivot								: packoffset(c40);
	float3 PreviousBonesPivot						: packoffset(c41);
#endif
};

cbuffer PerTechnique								: register(b0)
{
	float4 HighDetailRange							: packoffset(c0);	// loaded cells center in xy, size in zw
	float4 FogParam									: packoffset(c1);
	float4 FogNearColor								: packoffset(c2);
	float4 FogFarColor								: packoffset(c3);
};

cbuffer PerMaterial									: register(b1)
{
	float4 LeftEyeCenter							: packoffset(c0);
	float4 RightEyeCenter							: packoffset(c1);
	float4 TexcoordOffset							: packoffset(c2);
};

cbuffer PerGeometry									: register(b2)
{
	row_major float3x4 World						: packoffset(c0);
	row_major float3x4 PreviousWorld				: packoffset(c3);
	float4 EyePosition								: packoffset(c6);
	float4 LandBlendParams							: packoffset(c7);	// offset in xy, gridPosition in yw
	float4 TreeParams								: packoffset(c8);	// wind magnitude in y, amplitude in z, leaf frequency in w
	float2 WindTimers								: packoffset(c9);
	row_major float3x4 TextureProj					: packoffset(c10);
	float IndexScale								: packoffset(c13);
	float4 WorldMapOverlayParameters				: packoffset(c14);
};

#if defined (SKINNED)
cbuffer PreviousBonesBuffer							: register(b9)
{
	float4 PreviousBones[240]						: packoffset(c0);
}

cbuffer BonesBuffer									: register(b10)
{
	float4 Bones[240]								: packoffset(c0);
}
#endif

#if defined (TREE_ANIM)
float2 GetTreeShiftVector(float4 position, float4 color)
{
	precise float4 tmp1 = (TreeParams.w * TreeParams.y).xxxx * WindTimers.xxyy;
	precise float4 tmp2 = float4(0.1, 0.25, 0.1, 0.25) * tmp1 + dot(position.xyz, 1.0.xxx).xxxx;
	precise float4 tmp3 = abs(-1.0.xxxx + 2.0.xxxx * frac(0.5.xxxx + tmp2.xyzw));
	precise float4 tmp4 = (tmp3 * tmp3) * (3.0.xxxx - 2.0.xxxx * tmp3);
	return (tmp4.xz + 0.1.xx * tmp4.yw) * (TreeParams.z * color.w).xx;
}
#endif

#if defined (SKINNED)
float3x4 GetBoneMatrix(float4 bones[240], int4 actualIndices, float3 pivot, float4 weights)
{
	/*float3x4 result;
	for (int rowIndex = 0; rowIndex < 3; ++rowIndex)
	{
		float4 pivotRow = float4(0, 0, 0, pivot[rowIndex]);
		result[rowIndex] = 0.0.xxxx;
		for (int boneIndex = 0; boneIndex < 4; ++boneIndex)
		{
			result[rowIndex] += (bones[actualIndices[boneIndex] + rowIndex] - pivotRow) * weights[boneIndex];
		}
	}
	return result;*/

	float3x4 pivotMatrix = transpose(float4x3(0.0.xxx, 0.0.xxx, 0.0.xxx, pivot));

	float3x4 boneMatrix1 =
		float3x4(bones[actualIndices.x], bones[actualIndices.x + 1], bones[actualIndices.x + 2]);
	float3x4 boneMatrix2 =
		float3x4(bones[actualIndices.y], bones[actualIndices.y + 1], bones[actualIndices.y + 2]);
	float3x4 boneMatrix3 =
		float3x4(bones[actualIndices.z], bones[actualIndices.z + 1], bones[actualIndices.z + 2]);
	float3x4 boneMatrix4 =
		float3x4(bones[actualIndices.w], bones[actualIndices.w + 1], bones[actualIndices.w + 2]);

	float3x4 unitMatrix = float3x4(1.0.xxxx, 1.0.xxxx, 1.0.xxxx);
	float3x4 weightMatrix1 = unitMatrix * weights.x;
	float3x4 weightMatrix2 = unitMatrix * weights.y;
	float3x4 weightMatrix3 = unitMatrix * weights.z;
	float3x4 weightMatrix4 = unitMatrix * weights.w;

	return (boneMatrix1 - pivotMatrix) * weightMatrix1 +
	       (boneMatrix2 - pivotMatrix) * weightMatrix2 +
	       (boneMatrix3 - pivotMatrix) * weightMatrix3 +
	       (boneMatrix4 - pivotMatrix) * weightMatrix4;
}

float3x3 GetBoneRSMatrix(float4 bones[240], int4 actualIndices, float4 weights)
{
	float3x3 result;
	for (int rowIndex = 0; rowIndex < 3; ++rowIndex)
	{
		result[rowIndex] = weights.xxx * bones[actualIndices.x + rowIndex].xyz +
		                   weights.yyy * bones[actualIndices.y + rowIndex].xyz +
		                   weights.zzz * bones[actualIndices.z + rowIndex].xyz +
		                   weights.www * bones[actualIndices.w + rowIndex].xyz;
	}
	return result;
}
#endif

VS_OUTPUT main(VS_INPUT input)
{
	VS_OUTPUT vsout;

	precise float4 inputPosition = float4(input.Position.xyz, 1.0);

#if defined (LODLANDNOISE) || defined(LODLANDSCAPE)
	float4 rawWorldPosition = float4(mul(World, inputPosition), 1);
	float worldXShift = rawWorldPosition.x - HighDetailRange.x;
	float worldYShift = rawWorldPosition.y - HighDetailRange.y;
	if ((abs(worldXShift) < HighDetailRange.z) && (abs(worldYShift) < HighDetailRange.w))
	{
		inputPosition.z -= (230 + rawWorldPosition.z / 1e9);
	}
#endif
	
	precise float4 previousInputPosition = inputPosition;
	
#if defined(TREE_ANIM)
	precise float2 treeShiftVector = GetTreeShiftVector(input.Position, input.Color);
	float3 normal = -1.0.xxx + 2.0.xxx * input.Normal.xyz;
	
	inputPosition.xyz += normal.xyz * treeShiftVector.x;
	previousInputPosition.xyz += normal.xyz * treeShiftVector.y;
#endif

#if defined (SKINNED)
	precise int4 actualIndices = 765.01.xxxx * input.BoneIndices.xyzw;

	float3x4 previousWorldMatrix =
		GetBoneMatrix(PreviousBones, actualIndices, PreviousBonesPivot, input.BoneWeights);
	precise float4 previousWorldPosition =
		float4(mul(inputPosition, transpose(previousWorldMatrix)), 1);

	float3x4 worldMatrix = GetBoneMatrix(Bones, actualIndices, BonesPivot, input.BoneWeights);
	precise float4 worldPosition = float4(mul(inputPosition, transpose(worldMatrix)), 1);

	float4 viewPos = mul(ViewProj, worldPosition);
#else
	precise float4 previousWorldPosition = float4(mul(PreviousWorld, inputPosition), 1);
	precise float4 worldPosition = float4(mul(World, inputPosition), 1);
	precise float4x4 world4x4 = float4x4(World[0], World[1], World[2], float4(0, 0, 0, 1));
	precise float4x4 modelView = mul(ViewProj, world4x4);
	float4 viewPos = mul(modelView, inputPosition);
#endif

#if defined(OUTLINE) && !defined(MODELSPACENORMALS)
	float3 normal = normalize(-1.0.xxx + 2.0.xxx * input.Normal.xyz);
	float outlineShift = min(max(viewPos.z / 150, 1), 50);
	inputPosition.xyz += outlineShift * normal.xyz;
	previousInputPosition.xyz += outlineShift * normal.xyz;

#if defined (SKINNED)
	previousWorldPosition =
		float4(mul(inputPosition, transpose(previousWorldMatrix)), 1);
	worldPosition = float4(mul(inputPosition, transpose(worldMatrix)), 1);
	viewPos = mul(ViewProj, worldPosition);
#else
	previousWorldPosition = float4(mul(PreviousWorld, inputPosition), 1);
	worldPosition = float4(mul(World, inputPosition), 1);
	viewPos = mul(modelView, inputPosition);
#endif
#endif

	vsout.Position = viewPos;

#if defined (LODLANDNOISE) || defined (LODLANDSCAPE)
	vsout.Position.z += min(1, 1e-4 * max(0, viewPos.z - 70000)) * 0.5;
#endif
	
	float2 uv = input.TexCoord0.xy * TexcoordOffset.zw + TexcoordOffset.xy;
#if defined(LANDSCAPE)
	vsout.TexCoord0.zw = (uv * 0.010416667.xx + LandBlendParams.xy) * float2(1,-1) + float2(0,1);
#elif defined (PROJECTED_UV) && !defined(SKINNED)
	vsout.TexCoord0.z = mul(TextureProj[0], inputPosition); 
	vsout.TexCoord0.w = mul(TextureProj[1], inputPosition); 
#endif
	vsout.TexCoord0.xy = uv;
	
#if defined(ENVMAP) || defined(MULTI_LAYER_PARALLAX) || defined(SKINNED)
	vsout.InputPosition.xyz = worldPosition.xyz;
#elif defined (WORLD_MAP)
	vsout.InputPosition.xyz = WorldMapOverlayParameters.xyz + worldPosition.xyz;
#else
	vsout.InputPosition.xyz = inputPosition.xyz;
#endif

#if defined (SKINNED)
	float3x3 boneRSMatrix = GetBoneRSMatrix(Bones, actualIndices, input.BoneWeights);
#endif
		
#if !defined(MODELSPACENORMALS)
	float3x3 tbn = float3x3(
		float3(input.Position.w, input.Normal.w * 2 - 1, input.Bitangent.w * 2 - 1),
		input.Bitangent.xyz * 2.0.xxx + -1.0.xxx,
		input.Normal.xyz * 2.0.xxx + -1.0.xxx
	);
	float3x3 tbnTr = transpose(tbn);

#if defined (SKINNED)
	float3x3 worldTbnTr = transpose(mul(transpose(tbnTr), transpose(boneRSMatrix)));
	float3x3 worldTbnTrTr = transpose(worldTbnTr);
	worldTbnTrTr[0] = normalize(worldTbnTrTr[0]);
	worldTbnTrTr[1] = normalize(worldTbnTrTr[1]);
	worldTbnTrTr[2] = normalize(worldTbnTrTr[2]);
	worldTbnTr = transpose(worldTbnTrTr);
	vsout.TBN0.xyz = worldTbnTr[0];
	vsout.TBN1.xyz = worldTbnTr[1];
	vsout.TBN2.xyz = worldTbnTr[2];
#elif defined (ENVMAP) || defined(MULTI_LAYER_PARALLAX)
	vsout.TBN0.xyz = mul(tbn, World[0].xyz);
	vsout.TBN1.xyz = mul(tbn, World[1].xyz);
	vsout.TBN2.xyz = mul(tbn, World[2].xyz);
#else
	vsout.TBN0.xyz = tbnTr[0];
	vsout.TBN1.xyz = tbnTr[1];
	vsout.TBN2.xyz = tbnTr[2];
#endif
#elif defined (SKINNED)
	float3x3 boneRSMatrixTr = transpose(boneRSMatrix);
	float3x3 worldTbnTr = transpose(float3x3(normalize(boneRSMatrixTr[0]),
		normalize(boneRSMatrixTr[1]), normalize(boneRSMatrixTr[2])));

	vsout.TBN0.xyz = worldTbnTr[0];
	vsout.TBN1.xyz = worldTbnTr[1];
	vsout.TBN2.xyz = worldTbnTr[2];
#endif
	
#if defined(LANDSCAPE)
	vsout.LandBlendWeights1 = input.LandBlendWeights1;
	
	float2 gridOffset = LandBlendParams.zw - input.Position.xy;
	vsout.LandBlendWeights2.w = 1 - saturate(0.000375600968 * (9625.59961 - length(gridOffset)));	
	vsout.LandBlendWeights2.xyz = input.LandBlendWeights2.xyz;
#elif defined(PROJECTED_UV) && !defined(SKINNED)
	vsout.TexProj = TextureProj[2].xyz;
#endif
	
#if defined (HAS_VIEW_VECTOR)
#if defined(ENVMAP) || defined(MULTI_LAYER_PARALLAX) || defined(SKINNED)
	vsout.ViewVector = EyePosition.xyz - worldPosition.xyz;
#else
	vsout.ViewVector = EyePosition.xyz - input.Position.xyz;
#endif
#endif

#if defined (EYE)
	precise float4 modelEyeCenter = float4(LeftEyeCenter.xyz + input.EyeParameter.xxx * (RightEyeCenter.xyz - LeftEyeCenter.xyz), 1);
	vsout.EyeNormal.xyz = normalize(worldPosition.xyz - mul(modelEyeCenter, transpose(worldMatrix)));
#endif
	
#if defined (SKINNED)
	float3x3 ScreenNormalTransform = mul(ScreenProj, worldTbnTr);

	vsout.ScreenNormalTransform0.xyz = ScreenNormalTransform[0];
	vsout.ScreenNormalTransform1.xyz = ScreenNormalTransform[1];
	vsout.ScreenNormalTransform2.xyz = ScreenNormalTransform[2];
#else
	float3x4 transMat = mul(ScreenProj, World);
	
#if defined(MODELSPACENORMALS)
	vsout.ScreenNormalTransform0.xyz = transMat[0].xyz;
	vsout.ScreenNormalTransform1.xyz = transMat[1].xyz;
	vsout.ScreenNormalTransform2.xyz = transMat[2].xyz;
#else
	vsout.ScreenNormalTransform0.xyz = mul(transMat[0].xyz, transpose(tbn));
	vsout.ScreenNormalTransform1.xyz = mul(transMat[1].xyz, transpose(tbn));
	vsout.ScreenNormalTransform2.xyz = mul(transMat[2].xyz, transpose(tbn));
#endif
#endif

	vsout.WorldPosition = worldPosition;
	vsout.PreviousWorldPosition = previousWorldPosition;
	
#if defined (VC)
	vsout.Color = input.Color;
#else
	vsout.Color = 1.0.xxxx;
#endif	
	
    float fogColorParam = min(FogParam.w,
		exp2(FogParam.z * log2(saturate(length(viewPos.xyz) * FogParam.y - FogParam.x))));

    vsout.FogParam.xyz = lerp(FogNearColor.xyz, FogFarColor.xyz, fogColorParam);
	vsout.FogParam.w = fogColorParam;
	
    return vsout;
}
#endif

typedef VS_OUTPUT PS_INPUT;

struct PS_OUTPUT
{
    float4 Albedo									: SV_Target0;
    float4 MotionVectors							: SV_Target1;
    float4 ScreenSpaceNormals						: SV_Target2;
#if defined (SNOW)
	float2 SnowParameters							: SV_Target3;
#endif
};

#ifdef PSHADER
#if defined(LANDSCAPE)

SamplerState SampColorSampler						: register(s0);
SamplerState SampLandColor2Sampler					: register(s1);
SamplerState SampLandColor3Sampler					: register(s2);
SamplerState SampLandColor4Sampler					: register(s3);
SamplerState SampLandColor5Sampler					: register(s4);
SamplerState SampLandColor6Sampler					: register(s5);
SamplerState SampNormalSampler						: register(s7);
SamplerState SampLandNormal2Sampler					: register(s8);
SamplerState SampLandNormal3Sampler					: register(s9);
SamplerState SampLandNormal4Sampler					: register(s10);
SamplerState SampLandNormal5Sampler					: register(s11);
SamplerState SampLandNormal6Sampler					: register(s12);

#else 
SamplerState SampColorSampler						: register(s0);
SamplerState SampNormalSampler						: register(s1);

#if defined (MODELSPACENORMALS) && !defined(LODLANDNOISE)
SamplerState SampSpecularSampler					: register(s2);
#endif
#if defined (FACEGEN)
SamplerState SampTintSampler						: register(s3);
SamplerState SampDetailSampler						: register(s4);
#elif defined (PARALLAX)
SamplerState SampParallaxSampler					: register(s3);
#elif defined (PROJECTED_UV) && !defined(SPARKLE)
SamplerState SampProjDiffuseSampler					: register(s3);
#endif
#if defined (PBR)
SamplerState SampRMAOSampler						: register(s6);
#endif
#if (defined (ENVMAP) || defined(MULTI_LAYER_PARALLAX) || defined(SNOW_FLAG) || defined(EYE)) && !defined(FACEGEN)
SamplerState SampEnvSampler							: register(s4);
SamplerState SampEnvMaskSampler						: register(s5);
#endif
#if defined (GLOWMAP)
SamplerState SampGlowSampler						: register(s6);
#endif
#if defined (MULTI_LAYER_PARALLAX)
SamplerState SampLayerSampler						: register(s8);
#elif defined (PROJECTED_UV) && !defined(SPARKLE)
SamplerState SampProjNormalSampler					: register(s8);
#endif
#if defined (BACK_LIGHTING)
SamplerState SampBackLightSampler					: register(s9);
#endif
#if defined (PROJECTED_UV)
SamplerState SampProjDetailSampler					: register(s10);
#endif
#if defined (CHARACTER_LIGHT)
SamplerState SampCharacterLightSampler				: register(s11);
#elif defined (PROJECTED_UV)
SamplerState SampProjNoiseSampler					: register(s11);
#endif
#if defined (SOFT_LIGHTING) || defined(RIM_LIGHTING)
SamplerState SampRimSoftLightSampler				: register(s12);
#elif defined (WORLD_MAP)
SamplerState SampWorldMapOverlaySampler				: register(s12);
#endif
#if defined(WORLD_MAP) && (defined(LODLANDSCAPE) || defined(LODLANDNOISE))
SamplerState SampWorldMapOverlaySnowSampler			: register(s13);
#endif

#endif

#if defined(LOD_LAND_BLEND)
SamplerState SampLandLodBlend1Sampler				: register(s13);
SamplerState SampLandLodBlend2Sampler				: register(s15);
#elif defined(LODLANDNOISE)
SamplerState SampLandLodNoiseSampler				: register(s15);
#endif
#if defined (DEFSHADOW)
SamplerState SampShadowMaskSampler					: register(s14);
#endif

#if defined(LANDSCAPE)

Texture2D<float4> TexColorSampler					: register(t0);
Texture2D<float4> TexLandColor2Sampler				: register(t1);
Texture2D<float4> TexLandColor3Sampler				: register(t2);
Texture2D<float4> TexLandColor4Sampler				: register(t3);
Texture2D<float4> TexLandColor5Sampler				: register(t4);
Texture2D<float4> TexLandColor6Sampler				: register(t5);
Texture2D<float4> TexNormalSampler					: register(t7);
Texture2D<float4> TexLandNormal2Sampler				: register(t8);
Texture2D<float4> TexLandNormal3Sampler				: register(t9);
Texture2D<float4> TexLandNormal4Sampler				: register(t10);
Texture2D<float4> TexLandNormal5Sampler				: register(t11);
Texture2D<float4> TexLandNormal6Sampler				: register(t12);

#else 

Texture2D<float4> TexColorSampler					: register(t0);
Texture2D<float4> TexNormalSampler					: register(t1); // normal in xyz, glossiness in w if not modelspacenormal

#if defined (MODELSPACENORMALS) && !defined(LODLANDNOISE)
Texture2D<float4> TexSpecularSampler				: register(t2);
#endif
#if defined (FACEGEN)
Texture2D<float4> TexTintSampler					: register(t3);
Texture2D<float4> TexDetailSampler					: register(t4);
#elif defined (PARALLAX)
Texture2D<float4> TexParallaxSampler				: register(t3);
#elif defined (PROJECTED_UV) && !defined(SPARKLE)
Texture2D<float4> TexProjDiffuseSampler				: register(t3);
#endif
#if defined (PBR)
Texture2D<float4> TexRMAOSampler					: register(t6);
#endif
#if (defined (ENVMAP) || defined(MULTI_LAYER_PARALLAX) || defined(SNOW_FLAG) || defined(EYE)) && !defined(FACEGEN)
TextureCube<float4> TexEnvSampler					: register(t4);
Texture2D<float4> TexEnvMaskSampler					: register(t5);
#endif
#if defined (GLOWMAP)
Texture2D<float4> TexGlowSampler					: register(t6);
#endif
#if defined (MULTI_LAYER_PARALLAX)
Texture2D<float4> TexLayerSampler					: register(t8);
#elif defined (PROJECTED_UV) && !defined(SPARKLE)
Texture2D<float4> TexProjNormalSampler				: register(t8);
#endif
#if defined (BACK_LIGHTING)
Texture2D<float4> TexBackLightSampler				: register(t9);
#endif
#if defined (PROJECTED_UV)
Texture2D<float4> TexProjDetail						: register(t10);
#endif
#if defined (CHARACTER_LIGHT)
Texture2D<float4> TexCharacterLightSampler			: register(t11);
#elif defined (PROJECTED_UV)
Texture2D<float4> TexProjNoiseSampler				: register(t11);
#endif
#if defined (SOFT_LIGHTING) || defined(RIM_LIGHTING)
Texture2D<float4> TexRimSoftLightSampler			: register(t12);
#elif defined (WORLD_MAP)
Texture2D<float4> TexWorldMapOverlaySampler			: register(t12);
#endif
#if defined(WORLD_MAP) && (defined(LODLANDSCAPE) || defined(LODLANDNOISE))
Texture2D<float4> TexWorldMapOverlaySnowSampler		: register(t13);
#endif

#endif

#if defined(LOD_LAND_BLEND)
Texture2D<float4> TexLandLodBlend1Sampler			: register(t13);
Texture2D<float4> TexLandLodBlend2Sampler			: register(t15);
#elif defined(LODLANDNOISE)
Texture2D<float4> TexLandLodNoiseSampler			: register(t15);
#endif
#if defined (DEFSHADOW)
Texture2D<float4> TexShadowMaskSampler				: register(t14);
#endif

cbuffer PerFrame									: register(b12)
{
	float4 UnknownPerFrame1[12]						: packoffset(c0);
	row_major float4x4 ScreenProj					: packoffset(c12);
	row_major float4x4 PreviousScreenProj			: packoffset(c16);
	float4 UnknownPerFrame2[23]						: packoffset(c20);
	float4 UnknownPerFrame3[2]						: packoffset(c43);
};

cbuffer PerTechnique								: register(b0)
{
    float4 FogColor									: packoffset(c0); // Color in xyz, invFrameBufferRange in w
    float4 ColourOutputClamp						: packoffset(c1); // fLightingOutputColourClampPostLit in x, fLightingOutputColourClampPostEnv in y, fLightingOutputColourClampPostSpec in z
    float4 VPOSOffset								: packoffset(c2); // ???
};

cbuffer PerMaterial									: register(b1)
{
    float4 LODTexParams								: packoffset(c0); // TerrainTexOffset in xy, LodBlendingEnabled in z
    float4 TintColor								: packoffset(c1);
    float4 EnvmapData								: packoffset(c2); // fEnvmapScale in x, 1 or 0 in y depending of if has envmask
    float4 ParallaxOccData							: packoffset(c3);
    float4 SpecularColor							: packoffset(c4); // Shininess in w, color in xyz
    float4 SparkleParams							: packoffset(c5);
    float4 MultiLayerParallaxData					: packoffset(c6); // Layer thickness in x, refraction scale in y, uv scale in zw
    float4 LightingEffectParams						: packoffset(c7); // fSubSurfaceLightRolloff in x, fRimLightPower in y
    float4 IBLParams								: packoffset(c8);
	float4 LandscapeTexture1to4IsSnow				: packoffset(c9);
	float4 LandscapeTexture5to6IsSnow				: packoffset(c10); // bEnableSnowMask in z, inverse iLandscapeMultiNormalTilingFactor in w
	float4 LandscapeTexture1to4IsSpecPower			: packoffset(c11);
	float4 LandscapeTexture5to6IsSpecPower			: packoffset(c12);
	float4 SnowRimLightParameters					: packoffset(c13); // fSnowRimLightIntensity in x, fSnowGeometrySpecPower in y, fSnowNormalSpecPower in z, bEnableSnowRimLighting in w
    float4 CharacterLightParams						: packoffset(c14);
};

cbuffer PerGeometry									: register(b2)
{
    float3 DirLightDirection						: packoffset(c0);
    float3 DirLightColor							: packoffset(c1);
    float4 ShadowLightMaskSelect					: packoffset(c2);
    float4 MaterialData								: packoffset(c3); // envmapLODFade in x, specularLODFade in y, alpha in z
	float AlphaTestRef								: packoffset(c4);
    float3 EmitColor								: packoffset(c4.y); 
	float4 ProjectedUVParams						: packoffset(c6);
	float4 SSRParams								: packoffset(c7);
    float4 WorldMapOverlayParametersPS				: packoffset(c8);
	float4 ProjectedUVParams2						: packoffset(c9);
    float4 ProjectedUVParams3						: packoffset(c10); // fProjectedUVDiffuseNormalTilingScale in x, fProjectedUVNormalDetailTilingScale in y, EnableProjectedNormals in w
    row_major float3x4 DirectionalAmbient			: packoffset(c11);
    float4 AmbientSpecularTintAndFresnelPower		: packoffset(c14); // Fresnel power in z, color in xyz
    float4 PointLightPosition[7]					: packoffset(c15); // point light radius in w
    float4 PointLightColor[7]						: packoffset(c22);
    float2 NumLightNumShadowLight					: packoffset(c29);
};

cbuffer AlphaTestRefBuffer							: register(b11)
{
    float AlphaThreshold							: packoffset(c0);
}

float GetSoftLightMultiplier(float angle)
{
    float softLightParam = saturate((LightingEffectParams.x + angle) / (1 + LightingEffectParams.x));
	float arg1 = (softLightParam * softLightParam) * (3 - 2 * softLightParam);
    float clampedAngle = saturate(angle);
	float arg2 = (clampedAngle * clampedAngle) * (3 - 2 * clampedAngle);
    float softLigtMul = saturate(arg1 - arg2);
    return softLigtMul;
}

float GetRimLightMultiplier(float3 L, float3 V, float3 N)
{
    float NdotV = saturate(dot(N, V));
    return exp2(LightingEffectParams.y * log2(1 - NdotV)) * saturate(dot(V, -L));
}

float ProcessSparkleColor(float color)
{
	return exp2(SparkleParams.y * log2(min(1, abs(color))));
}

float3 GetLightSpecularInput(PS_INPUT input, float3 L, float3 V, float3 N, float3 lightColor, float shininess, float2 uv)
{
    float3 H = normalize(V + L);
#if defined (ANISO_LIGHTING)
	float3 AN = normalize(N * 0.5 + float3(input.TBN0.z, input.TBN1.z, input.TBN2.z));
	float LdotAN = dot(AN, L);
	float HdotAN = dot(AN, H);
	float HdotN = 1 - min(1, abs(LdotAN - HdotAN));
#else
    float HdotN = saturate(dot(H, N));
#endif
#if defined (SPECULAR)
    float lightColorMultiplier = exp2(shininess * log2(HdotN));
#elif defined (SPARKLE)
	float lightColorMultiplier = 0;
#else
	float lightColorMultiplier = HdotN;
#endif
#if defined (ANISO_LIGHTING)
	lightColorMultiplier *= 0.7 * max(0, L.z);
#endif
#if defined (SPARKLE) && !defined(SNOW)
	float3 sparkleUvScale = exp2(float3(1.3, 1.6, 1.9) * log2(abs(SparkleParams.x)).xxx);

	float sparkleColor1 = TexProjDetail.Sample(SampProjDetailSampler, uv * sparkleUvScale.xx).z;
	float sparkleColor2 = TexProjDetail.Sample(SampProjDetailSampler, uv * sparkleUvScale.yy).z;
	float sparkleColor3 = TexProjDetail.Sample(SampProjDetailSampler, uv * sparkleUvScale.zz).z;
	float sparkleColor = ProcessSparkleColor(sparkleColor1)
		+ ProcessSparkleColor(sparkleColor2)
		+ ProcessSparkleColor(sparkleColor3);
	float VdotN = dot(V, N);
	V += N * -(2 * VdotN);
	float sparkleMultiplier = exp2(SparkleParams.w * log2(saturate(dot(V, -L)))) * (SparkleParams.z * sparkleColor);
	sparkleMultiplier = sparkleMultiplier >= 0.5 ? 1 : 0;
	lightColorMultiplier += sparkleMultiplier * HdotN;
#endif
    return lightColor * lightColorMultiplier.xxx;
}

float3 TransformNormal(float3 normal)
{
    return normal * 2 + -1.0.xxx;
}

float3 fresnelSchlick(float cosTheta, float3 F0)
{
    return F0 + (1 - F0) * pow(saturate(1 - cosTheta), 5);
}

static const float PI = 3.14159265;

float DistributionGGX(float NdotH, float roughness)
{
    float a = roughness * roughness;
    float a2 = a * a;
    float NdotH2 = NdotH * NdotH;
	
    float num = a2;
    float denom = (NdotH2 * (a2 - 1) + 1);
    denom = PI * denom * denom;
	
    return num / denom;
}

float GeometrySchlickGGX(float cosTheta, float roughness)
{
    float r = (roughness + 1);
    float k = (r * r) / 8;

    float num = cosTheta;
    float denom = cosTheta * (1 - k) + k;
	
    return num / denom;
}

float GeometrySmith(float NdotV, float NdotL, float roughness)
{
    float ggxV = GeometrySchlickGGX(NdotV, roughness);
    float ggxL = GeometrySchlickGGX(NdotL, roughness);
	
    return ggxV * ggxL;
}

float OrenNayarDiffuseCoefficient(float roughness, float3 N, float3 L, float3 V, float NdotL, float NdotV)
{
    float gamma = dot(V - N * NdotV, L - N * NdotL);
    float a = roughness * roughness;
    float A = 1 - 0.5 * (a / (a + 0.57));
    float B = 0.45 * (a / (a + 0.09));
    float C = sqrt((1 - NdotV * NdotV) * (1 - NdotL * NdotL)) / max(NdotV, NdotL);
    return (A + B * max(0.0f, gamma) * C) / PI;

}

float3 GetLightRadiance(float3 N, float3 L, float3 V, float3 F0, float3 originalRadiance, float3 albedo, float roughness, float metallic)
{
    float3 radiance = PI * originalRadiance;
	
    float3 H = normalize(V + L);
    float NdotL = dot(N, L);
    float NdotV = dot(N, V);
    float HdotV = dot(H, V);
    float NdotH = dot(N, H);
        
    float NDF = DistributionGGX(saturate(NdotH), roughness);
    float G = GeometrySmith(saturate(NdotV), saturate(NdotL), roughness);
    float3 F = fresnelSchlick(saturate(HdotV), F0);
        
    float3 kD = (1 - F) * (1 - metallic);
        
    float3 numerator = NDF * G * F;
    float denominator = 4 * saturate(NdotV) * saturate(NdotL) + 0.0001;
    float3 specular = numerator / denominator;
            
    float diffuseValue = OrenNayarDiffuseCoefficient(roughness, N, L, V, NdotL, NdotV);
    return (kD * diffuseValue * albedo + specular) * saturate(NdotL) * radiance;
}

float GetLodLandBlendParameter(float3 color)
{
	float result = saturate(1.6666666 * (dot(color, 0.55.xxx) - 0.4));
	result = ((result * result) * (3 - result * 2));
#if !defined (WORLD_MAP)
	result *= 0.8;
#endif
	return result;
}

float GetLodLandBlendMultiplier(float parameter, float mask)
{
	return 0.8333333 * (parameter * (0.37 - mask) + mask) + 0.37;
}

float GetLandSnowMaskValue(float alpha)
{
	return alpha * LandscapeTexture5to6IsSnow.z + (1 + -LandscapeTexture5to6IsSnow.z);
}

float3 GetLandNormal(float landSnowMask, float3 normal, float2 uv, SamplerState sampNormal, Texture2D<float4> texNormal)
{
	float3 landNormal = TransformNormal(normal);
#if defined (SNOW)
	if (landSnowMask > 1e-5 && LandscapeTexture5to6IsSnow.w != 1.0)
	{
		float3 snowNormal =
			float3(-1, -1, 1) *
			TransformNormal(texNormal.Sample(sampNormal, LandscapeTexture5to6IsSnow.ww * uv).xyz);
		landNormal.z += 1;
		float normalProjection = dot(landNormal, snowNormal);
		snowNormal = landNormal * normalProjection.xxx - snowNormal * landNormal.z;
		return normalize(snowNormal);
	}
	else
	{
		return landNormal;
	}
#else
	return landNormal;
#endif
}

#if defined (SNOW)
float3 GetSnowSpecularColor(PS_INPUT input, float3 modelNormal, float3 viewDirection)
{
	if (SnowRimLightParameters.w > 1e-5)
	{
#if defined (MODELSPACENORMALS) && !defined(SKINNED)
#if defined (HAS_VIEW_VECTOR)
		float3 modelGeometryNormal = float3(0, 0, 1);
#else
		float3 modelGeometryNormal = 0.333333333.xxx;
#endif
#else
		float3 modelGeometryNormal = normalize(float3(input.TBN0.z, input.TBN1.z, input.TBN2.z));
#endif
		float normalFactor = 1 - saturate(dot(modelNormal, viewDirection));
		float geometryNormalFactor = 1 - saturate(dot(modelGeometryNormal, viewDirection));
		return (SnowRimLightParameters.x
			* (exp2(SnowRimLightParameters.y * log2(geometryNormalFactor))
				* exp2(SnowRimLightParameters.z * log2(normalFactor)))).xxx;
	}
	else
	{
		return 0.0.xxx;
	}
}
#endif

#if defined (FACEGEN)
float3 GetFacegenBaseColor(float3 rawBaseColor, float2 uv)
{
	float3 detailColor = TexDetailSampler.Sample(SampDetailSampler, uv).xyz;
	detailColor = float3(3.984375, 3.984375, 3.984375) * (float3(0.00392156886, 0, 0.00392156886) + detailColor);
	float3 tintColor = TexTintSampler.Sample(SampTintSampler, uv).xyz;
	tintColor = tintColor * rawBaseColor * 2.0.xxx;
	tintColor = tintColor - tintColor * rawBaseColor;
	return (rawBaseColor * rawBaseColor + tintColor) * detailColor;
}
#endif

#if defined (FACEGEN_RGB_TINT)
float3 GetFacegenRGBTintBaseColor(float3 rawBaseColor, float2 uv)
{
	float3 tintColor = TintColor.xyz * rawBaseColor * 2.0.xxx;
	tintColor = tintColor - tintColor * rawBaseColor;
	return float3(1.01171875, 0.99609375, 1.01171875) * (rawBaseColor * rawBaseColor + tintColor);
}
#endif

#if defined (WORLD_MAP)
float3 GetWorldMapNormal(PS_INPUT input, float3 rawNormal, float3 baseColor)
{
	float3 normal = normalize(rawNormal);
#if defined (MODELSPACENORMALS)
	float3 worldMapNormalSrc = normal.xyz;
#else
	float3 worldMapNormalSrc = float3(input.TBN0.z, input.TBN1.z, input.TBN2.z);
#endif
	float3 worldMapNormal = 7.0.xxx * (-0.2.xxx + abs(normalize(worldMapNormalSrc)));
	worldMapNormal = max(0.01.xxx, worldMapNormal * worldMapNormal * worldMapNormal);
	worldMapNormal /= dot(worldMapNormal, 1.0.xxx);
	float3 worldMapColor1 = TexWorldMapOverlaySampler.Sample(SampWorldMapOverlaySampler, WorldMapOverlayParametersPS.xx * input.InputPosition.yz).xyz;
	float3 worldMapColor2 = TexWorldMapOverlaySampler.Sample(SampWorldMapOverlaySampler, WorldMapOverlayParametersPS.xx * input.InputPosition.xz).xyz;
	float3 worldMapColor3 = TexWorldMapOverlaySampler.Sample(SampWorldMapOverlaySampler, WorldMapOverlayParametersPS.xx * input.InputPosition.xy).xyz;
#if defined (LODLANDNOISE) || defined (LODLANDSCAPE)
	float3 worldMapSnowColor1 = TexWorldMapOverlaySnowSampler.Sample(SampWorldMapOverlaySnowSampler, WorldMapOverlayParametersPS.ww * input.InputPosition.yz).xyz;
	float3 worldMapSnowColor2 = TexWorldMapOverlaySnowSampler.Sample(SampWorldMapOverlaySnowSampler, WorldMapOverlayParametersPS.ww * input.InputPosition.xz).xyz;
	float3 worldMapSnowColor3 = TexWorldMapOverlaySnowSampler.Sample(SampWorldMapOverlaySnowSampler, WorldMapOverlayParametersPS.ww * input.InputPosition.xy).xyz;
#endif
	float3 worldMapColor = worldMapNormal.xxx * worldMapColor1 + worldMapNormal.yyy * worldMapColor2 + worldMapNormal.zzz * worldMapColor3;
#if defined (LODLANDNOISE) || defined (LODLANDSCAPE)
	float3 worldMapSnowColor = worldMapSnowColor1 * worldMapNormal.xxx + worldMapSnowColor2 * worldMapNormal.yyy + worldMapSnowColor3 * worldMapNormal.zzz;
	float snowMultiplier = GetLodLandBlendParameter(baseColor);
	worldMapColor = snowMultiplier * (worldMapSnowColor - worldMapColor) + worldMapColor;
#endif
	worldMapColor = normalize(2.0.xxx * (-0.5.xxx + (worldMapColor)));
#if defined (LODLANDNOISE) || defined (LODLANDSCAPE)
	float worldMapLandTmp = saturate(19.9999962 * (rawNormal.z - 0.95));
	worldMapLandTmp = saturate(-(worldMapLandTmp * worldMapLandTmp) * (worldMapLandTmp * -2 + 3) + 1.5);
	float3 worldMapLandTmp1 = normalize(normal.zxy * float3(1, 0, 0) - normal.yzx * float3(0, 0, 1));
	float3 worldMapLandTmp2 = normalize(worldMapLandTmp1.yzx * normal.zxy - worldMapLandTmp1.zxy * normal.yzx);
	float3 worldMapLandTmp3 = normalize(worldMapColor.xxx * worldMapLandTmp1 + worldMapColor.yyy * worldMapLandTmp2 + worldMapColor.zzz * normal.xyz);
	float worldMapLandTmp4 = dot(worldMapLandTmp3, worldMapLandTmp3);
	if (worldMapLandTmp4 > 0.999 && worldMapLandTmp4 < 1.001)
	{
		normal.xyz = worldMapLandTmp * (worldMapLandTmp3 - normal.xyz) + normal.xyz;
	}
#else
	normal.xyz = normalize(
		WorldMapOverlayParametersPS.zzz * (rawNormal.xyz - worldMapColor.xyz) + worldMapColor.xyz);
#endif
	return normal;
}

float3 GetWorldMapBaseColor(float3 originalBaseColor, float3 rawBaseColor, float texProjTmp)
{
#if defined(LODOBJECTS) && !defined(PROJECTED_UV)
	return rawBaseColor;
#endif
#if defined (LODLANDSCAPE) || defined(LODOBJECTSHD) || defined(LODLANDNOISE)
	float lodMultiplier = GetLodLandBlendParameter(originalBaseColor.xyz);
#elif defined (LODOBJECTS) && defined(PROJECTED_UV)
	float lodMultiplier = saturate(10 * texProjTmp);
#else
	float lodMultiplier = 1;
#endif
#if defined (LODOBJECTS)
	float4 lodColorMul = lodMultiplier.xxxx * float4(0.269999981, 0.281000018, 0.441000015, 0.441000015) + float4(0.0780000091, 0.09799999, -0.0349999964, 0.465000004);
	float4 lodColor = lodColorMul.xyzw * 2.0.xxxx;
	bool useLodColorZ = lodColorMul.w > 0.5;
	lodColor.xyz = max(lodColor.xyz, rawBaseColor.xyz);
	lodColor.w = useLodColorZ ? lodColor.z : min(lodColor.w, rawBaseColor.z);
	return (0.5 * lodMultiplier).xxx * (lodColor.xyw - rawBaseColor.xyz) + rawBaseColor;
#else
	float4 lodColorMul = lodMultiplier.xxxx * float4(0.199999988, 0.441000015, 0.269999981, 0.281000018) + float4(0.300000012, 0.465000004, 0.0780000091, 0.09799999);
	float3 lodColor = lodColorMul.zwy * 2.0.xxx;
	lodColor.xy = max(lodColor.xy, rawBaseColor.xy);
	lodColor.z = lodColorMul.y > 0.5 ? max((lodMultiplier * 0.441 + -0.0349999964) * 2, rawBaseColor.z) : min(lodColor.z, rawBaseColor.z);
	return lodColorMul.xxx * (lodColor - rawBaseColor.xyz) + rawBaseColor;
#endif
}
#endif

float GetSnowParameterY(float texProjTmp, float alpha)
{
#if defined (BASE_OBJECT_IS_SNOW)
	return min(1, texProjTmp + alpha);
#else
	return texProjTmp;
#endif
}

PS_OUTPUT main(PS_INPUT input)
{
    PS_OUTPUT psout;

#if defined (SKINNED) || !defined (MODELSPACENORMALS)
	float3x3 tbn = float3x3(input.TBN0.xyz, input.TBN1.xyz, input.TBN2.xyz);
#endif

#if defined (LANDSCAPE)
	float shininess = dot(input.LandBlendWeights1, LandscapeTexture1to4IsSpecPower) 
		+ input.LandBlendWeights2.x * LandscapeTexture5to6IsSpecPower.x 
		+ input.LandBlendWeights2.y * LandscapeTexture5to6IsSpecPower.y;
#else
	float shininess = SpecularColor.w;
#endif
	
#if defined (HAS_VIEW_VECTOR)
//#if defined (SKINNED)
//	float3 viewDirection = normalize(mul(tbn, input.ViewVector.xyz));
//#else
    float3 viewDirection = normalize(input.ViewVector.xyz);
//#endif
#else
	float3 viewDirection = 0.57735026.xxx;
#endif

    float2 uv = input.TexCoord0.xy;
#if defined (PARALLAX)
	float height = TexParallaxSampler.Sample(SampParallaxSampler, uv).x * 0.08 - 0.04;
	uv += viewDirection.xy * height;
#endif

#if defined (SNOW)
	bool useSnowSpecular = true;
#else
	bool useSnowSpecular = false;
#endif

#if defined(SPARKLE) || !defined(PROJECTED_UV)
	bool useSnowDecalSpecular = true;
#else
	bool useSnowDecalSpecular = false;
#endif
	
	float2 diffuseUv = uv;
#if defined (SPARKLE)
	diffuseUv = ProjectedUVParams2.yy * input.TexCoord0.zw;
#endif
    float4 rawBaseColor = TexColorSampler.Sample(SampColorSampler, diffuseUv);
	float4 baseColor = rawBaseColor;
	float landSnowMask1 = GetLandSnowMaskValue(baseColor.w);
	float4 normalColor = TexNormalSampler.Sample(SampNormalSampler, uv);

    float4 normal = normalColor;
#if defined (MODELSPACENORMALS)
#if defined (LODLANDNOISE)
	normal.xyz = normal.xzy - 0.5.xxx;
	float lodLandNoiseParameter = GetLodLandBlendParameter(baseColor.xyz);
	float noise = TexLandLodNoiseSampler.Sample(SampLandLodNoiseSampler, uv * 3.0.xx).x;
	float lodLandNoiseMultiplier = GetLodLandBlendMultiplier(lodLandNoiseParameter, noise);
	baseColor.xyz *= lodLandNoiseMultiplier;
	normal.xyz *= 2;
	normal.w = 1;
	float glossiness = 0;
#elif defined (LODLANDSCAPE)
	normal.xyz = 2.0.xxx * (-0.5.xxx + normal.xzy);
	normal.w = 1;
	float glossiness = 0;
#else
	normal.xyz = normal.xzy * 2.0.xxx + -1.0.xxx;
	normal.w = 1;
	float glossiness = TexSpecularSampler.Sample(SampSpecularSampler, uv).x;
#endif
#elif (defined (SNOW) && defined(LANDSCAPE))
	normal.xyz = GetLandNormal(landSnowMask1, normal.xyz, uv, SampNormalSampler, TexNormalSampler);
	float glossiness = normal.w;
#else
    normal.xyz = TransformNormal(normal.xyz);
    float glossiness = normal.w;
#endif	

#if defined (WORLD_MAP)
	normal.xyz = GetWorldMapNormal(input, normal.xyz, rawBaseColor.xyz);
#endif
	
#if defined (PBR)
    //float3 rmaoColor = TexRMAOSampler.Sample(SampRMAOSampler, uv).xyz;
    //float roughness = rmaoColor.x;
    //float metallic = rmaoColor.y;
    //float ao = rmaoColor.z;
	
    float roughness = 1 - glossiness;
    float metallic = 0;
#if defined (ENVMAP)
	float envMaskColor = TexEnvMaskSampler.Sample(SampEnvMaskSampler, uv).x;
	metallic = envMaskColor * EnvmapData.x;
#endif
    float ao = 1;
	
    float3 F0 = 0.04.xxx;
    F0 = lerp(F0, baseColor.xyz, metallic);
	
    float3 totalRadiance = 0.0.xxx;
#endif
	
#if defined (FACEGEN)
	baseColor.xyz = GetFacegenBaseColor(baseColor.xyz, uv);
#elif defined(FACEGEN_RGB_TINT)
	baseColor.xyz = GetFacegenRGBTintBaseColor(baseColor.xyz, uv);
#endif
	
#if defined (LANDSCAPE)
	float4 landColor2 = TexLandColor2Sampler.Sample(SampLandColor2Sampler, uv);
	float landSnowMask2 = GetLandSnowMaskValue(landColor2.w);
	baseColor = input.LandBlendWeights1.xxxx * baseColor + input.LandBlendWeights1.yyyy * landColor2;
	float4 landNormal2 = TexLandNormal2Sampler.Sample(SampLandNormal2Sampler, uv);
	landNormal2.xyz = GetLandNormal(landSnowMask2, landNormal2.xyz, uv, SampLandNormal2Sampler, TexLandNormal2Sampler);
	normal.xyz = input.LandBlendWeights1.xxx * normal.xyz + input.LandBlendWeights1.yyy * landNormal2.xyz;
	glossiness = input.LandBlendWeights1.x * glossiness + input.LandBlendWeights1.y * landNormal2.w;
	
	float4 landColor3 = TexLandColor3Sampler.Sample(SampLandColor3Sampler, uv);
	float landSnowMask3 = GetLandSnowMaskValue(landColor3.w);
	baseColor += input.LandBlendWeights1.zzzz * landColor3;
	float4 landNormal3 = TexLandNormal3Sampler.Sample(SampLandNormal3Sampler, uv);
	landNormal3.xyz = GetLandNormal(landSnowMask3, landNormal3.xyz, uv, SampLandNormal3Sampler, TexLandNormal3Sampler);
	normal.xyz += input.LandBlendWeights1.zzz * landNormal3.xyz;
	glossiness += input.LandBlendWeights1.z * landNormal3.w;
	
	float4 landColor4 = TexLandColor4Sampler.Sample(SampLandColor4Sampler, uv);
	float landSnowMask4 = GetLandSnowMaskValue(landColor4.w);
	baseColor += input.LandBlendWeights1.wwww * landColor4;
	float4 landNormal4 = TexLandNormal4Sampler.Sample(SampLandNormal4Sampler, uv);
	landNormal4.xyz = GetLandNormal(landSnowMask4, landNormal4.xyz, uv, SampLandNormal4Sampler, TexLandNormal4Sampler);
	normal.xyz += input.LandBlendWeights1.www * landNormal4.xyz;
	glossiness += input.LandBlendWeights1.w * landNormal4.w;
	
	float4 landColor5 = TexLandColor5Sampler.Sample(SampLandColor5Sampler, uv);
	float landSnowMask5 = GetLandSnowMaskValue(landColor5.w);
	baseColor += input.LandBlendWeights2.xxxx * landColor5;
	float4 landNormal5 = TexLandNormal5Sampler.Sample(SampLandNormal5Sampler, uv);
	landNormal5.xyz = GetLandNormal(landSnowMask5, landNormal5.xyz, uv, SampLandNormal5Sampler, TexLandNormal5Sampler);
	normal.xyz += input.LandBlendWeights2.xxx * landNormal5.xyz;
	glossiness += input.LandBlendWeights2.x * landNormal5.w;
	
	float4 landColor6 = TexLandColor6Sampler.Sample(SampLandColor6Sampler, uv);
	float landSnowMask6 = GetLandSnowMaskValue(landColor6.w);
	baseColor += input.LandBlendWeights2.yyyy * landColor6;
	float4 landNormal6 = TexLandNormal6Sampler.Sample(SampLandNormal6Sampler, uv);
	landNormal6.xyz = GetLandNormal(landSnowMask6, landNormal6.xyz, uv, SampLandNormal6Sampler, TexLandNormal6Sampler);
	normal.xyz += input.LandBlendWeights2.yyy * landNormal6.xyz;
	glossiness += input.LandBlendWeights2.y * landNormal6.w;
	
#if defined (LOD_LAND_BLEND)
	float4 lodBlendColor = TexLandLodBlend1Sampler.Sample(SampLandLodBlend1Sampler, input.TexCoord0.zw);
#endif

#if defined (SNOW)
	float landSnowMask = LandscapeTexture1to4IsSnow.x * input.LandBlendWeights1.x * landSnowMask1
		+ LandscapeTexture1to4IsSnow.y * input.LandBlendWeights1.y * landSnowMask2
		+ LandscapeTexture1to4IsSnow.z * input.LandBlendWeights1.z * landSnowMask3
		+ LandscapeTexture1to4IsSnow.w * input.LandBlendWeights1.w * landSnowMask4
		+ LandscapeTexture5to6IsSnow.x * input.LandBlendWeights2.x * landSnowMask5
		+ LandscapeTexture5to6IsSnow.y * input.LandBlendWeights2.y * landSnowMask6;
#endif

#if defined (LOD_LAND_BLEND)
	float lodBlendTmp = GetLodLandBlendParameter(lodBlendColor.xyz);
	float lodBlendMask = TexLandLodBlend2Sampler.Sample(SampLandLodBlend2Sampler, 3.0.xx * input.TexCoord0.zw).x;
	float lodBlendMul1 = GetLodLandBlendMultiplier(lodBlendTmp, lodBlendMask);
	float lodBlendMul2 = LODTexParams.z * input.LandBlendWeights2.w;
	baseColor.w = 0;
	baseColor = lodBlendMul2.xxxx * (lodBlendColor * lodBlendMul1.xxxx - baseColor) + baseColor;
	normal.xyz = lodBlendMul2.xxx * (float3(0, 0, 1) - normal.xyz) + normal.xyz;
	glossiness += lodBlendMul2 * -glossiness;
#endif

#if defined (SNOW)
	useSnowSpecular = landSnowMask > 0;
#endif
#endif
	
#if defined (BACK_LIGHTING)
	float4 backLightColor = TexBackLightSampler.Sample(SampBackLightSampler, uv);
#endif
#if defined (SOFT_LIGHTING) || defined(RIM_LIGHTING)
	float4 rimSoftLightColor = TexRimSoftLightSampler.Sample(SampRimSoftLightSampler, uv);
#endif
	
    float numLights = min(7, NumLightNumShadowLight.x);
#if defined (DEFSHADOW)
	float numShadowLights = min(4, NumLightNumShadowLight.y);
#endif
	
#if defined (MODELSPACENORMALS) && !defined (SKINNED)
	float4 modelNormal = normal;
#else
    float4 modelNormal = float4(normalize(mul(tbn, normal.xyz)), 1);

#if defined (SPARKLE)
	float3 projectedNormal = normalize(mul(tbn, float3(ProjectedUVParams2.xx * normal.xy, normal.z)));
#endif
#endif
	
    float2 baseShadowUV = 1.0.xx;
#if defined (DEFSHADOW)
	float4 shadowColor;
#if !defined (SHADOW_DIR)
	if (numShadowLights > 0)
#endif
	{
		baseShadowUV = input.Position.xy * UnknownPerFrame3[1].xy;
		float2 shadowUV = min(float2(UnknownPerFrame3[1].z, UnknownPerFrame3[0].y), max(0.0.xx, UnknownPerFrame3[0].xy * (baseShadowUV * VPOSOffset.xy + VPOSOffset.zw)));
		shadowColor = TexShadowMaskSampler.Sample(SampShadowMaskSampler, shadowUV);
	}
#if !defined (SHADOW_DIR)
	else
	{
		shadowColor = 1.0.xxxx;
	}
#endif
#endif

	float texProjTmp = 0;

#if defined (PROJECTED_UV)
	float2 projNoiseUv = ProjectedUVParams.zz * input.TexCoord0.zw;
	float projNoise = TexProjNoiseSampler.Sample(SampProjNoiseSampler, projNoiseUv).x;
	float3 texProj = normalize(input.TexProj);
#if defined (TREE_ANIM) || defined (LODOBJECTSHD)
	float vertexAlpha = 1;
#else
	float vertexAlpha = input.Color.w;
#endif
	texProjTmp = -ProjectedUVParams.x * projNoise + (dot(modelNormal.xyz, texProj) * vertexAlpha - ProjectedUVParams.w);
#if defined (LODOBJECTSHD)
	texProjTmp += (-0.5 + input.Color.w) * 2.5;
#endif
#if defined (SPARKLE)
	if (texProjTmp < 0)
	{
		discard;
	}

	modelNormal.xyz = projectedNormal;
#if defined (SNOW)
	psout.SnowParameters.y = 1;
#endif
#else
	if (ProjectedUVParams3.w > 0.5)
	{
		float2 projNormalUv = ProjectedUVParams3.x * projNoiseUv;
		float3 projNormal = TransformNormal(TexProjNormalSampler.Sample(SampProjNormalSampler, projNormalUv).xyz);
		float2 projDetailUv = ProjectedUVParams3.y * projNoiseUv;
		float3 projDetail = TexProjDetail.Sample(SampProjDetailSampler, projDetailUv).xyz;
		float3 texProjTmp3 = (projDetail * 2.0.xxx + float3(projNormal.xy, -1));
		texProjTmp3.xy += -1.0.xx;
		texProjTmp3.z = projNormal.z * texProjTmp3.z;
		float3 finalProjNormal = normalize(texProjTmp3);
		float3 projDiffuse = TexProjDiffuseSampler.Sample(SampProjDiffuseSampler, projNormalUv).xyz;
		float texProjTmp1 = saturate(5 * (0.1 + texProjTmp));
		float texProjTmp2 = (texProjTmp1 * -2 + 3) * (texProjTmp1 * texProjTmp1);
		normal.xyz = texProjTmp2.xxx * (finalProjNormal - normal.xyz) + normal.xyz;
		baseColor.xyz = texProjTmp2.xxx * (projDiffuse * ProjectedUVParams2.xyz - baseColor.xyz) + baseColor.xyz;

		useSnowDecalSpecular = true;
#if defined (SNOW)
		psout.SnowParameters.y = GetSnowParameterY(texProjTmp2, baseColor.w);
#endif
	}
	else
	{
		if (texProjTmp > 0)
		{
			baseColor.xyz = ProjectedUVParams2.xyz;
			useSnowDecalSpecular = true;
#if defined (SNOW)
			psout.SnowParameters.y = GetSnowParameterY(texProjTmp, baseColor.w);
#endif
		}
		else
		{
#if defined (SNOW)
			psout.SnowParameters.y = 0;
#endif
		}
	}

#if defined (SPECULAR)
	useSnowSpecular = useSnowDecalSpecular;
#endif
#endif

#elif defined (SNOW)
#if defined (LANDSCAPE)
	psout.SnowParameters.y = landSnowMask;
#else
	psout.SnowParameters.y = baseColor.w;
#endif
#endif

#if defined (WORLD_MAP)
	baseColor.xyz = GetWorldMapBaseColor(rawBaseColor.xyz, baseColor.xyz, texProjTmp);
#endif

	float3 dirLightColor = DirLightColor.xyz;
	float3 nsDirLightColor = dirLightColor;
#if defined (DEFSHADOW) && defined (SHADOW_DIR)
	dirLightColor *= shadowColor.xxx;
#endif
	
#if defined(PBR)
    totalRadiance += GetLightRadiance(modelNormal.xyz, DirLightDirection.xyz, viewDirection, F0, dirLightColor.xyz, baseColor.xyz, roughness, metallic);
#else
	float3 diffuseColor = 0.0.xxx;
	float3 specularColor = 0.0.xxx;
	
	float3 lightsDiffuseColor = 0.0.xxx;
	float3 lightsSpecularColor = 0.0.xxx;
	
	float dirLightAngle = dot(modelNormal.xyz, DirLightDirection.xyz);
	float3 dirDiffuseColor = dirLightColor * saturate(dirLightAngle.xxx);
	
#if defined (SOFT_LIGHTING)
	lightsDiffuseColor += nsDirLightColor.xyz * GetSoftLightMultiplier(dirLightAngle) * rimSoftLightColor.xyz;
#endif
	
#if defined (RIM_LIGHTING)
	lightsDiffuseColor += nsDirLightColor.xyz * GetRimLightMultiplier(DirLightDirection, viewDirection, modelNormal.xyz) * rimSoftLightColor.xyz;
#endif
	
#if defined (BACK_LIGHTING)
	lightsDiffuseColor += nsDirLightColor.xyz * (saturate(-dirLightAngle) * backLightColor.xyz);
#endif

	if (useSnowSpecular && useSnowDecalSpecular)
	{
#if defined (SNOW)
		lightsSpecularColor = GetSnowSpecularColor(input, modelNormal.xyz, viewDirection);
#endif
	}
	else
	{
#if defined (SPECULAR) || defined (SPARKLE)
		lightsSpecularColor = GetLightSpecularInput(input, DirLightDirection, viewDirection, modelNormal.xyz, dirLightColor.xyz, shininess, uv);
#endif
	}
	
	lightsDiffuseColor += dirDiffuseColor;
#endif
		
    if (numLights > 0)
    {
        for (float lightIndex = 0; lightIndex < numLights; ++lightIndex)
        {
#if defined (DEFSHADOW)
			float shadowComponent;
			if (lightIndex < numShadowLights)
			{
				shadowComponent = shadowColor[ShadowLightMaskSelect[lightIndex]];
			}
			else
			{
				shadowComponent = 1;
			}
#endif
			int intLightIndex = lightIndex;
            float3 lightDirection = PointLightPosition[intLightIndex].xyz - input.InputPosition.xyz;
            float lightDist = length(lightDirection);
            float intensityFactor = saturate(lightDist / PointLightPosition[intLightIndex].w);
            float intensityMultiplier = 1 - intensityFactor * intensityFactor;
			
            float3 lightColor = PointLightColor[intLightIndex].xyz;
			float3 nsLightColor = lightColor;
#if defined (DEFSHADOW)
			lightColor *= shadowComponent.xxx;
#endif
			
            float3 normalizedLightDirection = normalize(lightDirection);
			
#if defined(PBR)
            totalRadiance += GetLightRadiance(modelNormal.xyz, normalizedLightDirection, viewDirection, F0, lightColor * intensityMultiplier.xxx, baseColor.xyz, roughness, metallic);
#else
			float lightAngle = dot(modelNormal.xyz, normalizedLightDirection.xyz);
			float3 lightDiffuseColor = lightColor * saturate(lightAngle.xxx);
			
#if defined (SOFT_LIGHTING)
			lightDiffuseColor += nsLightColor * GetSoftLightMultiplier(dot(modelNormal.xyz, lightDirection.xyz)) * rimSoftLightColor.xyz;
#endif
			
#if defined (RIM_LIGHTING)
			lightDiffuseColor += nsLightColor * GetRimLightMultiplier(normalizedLightDirection, viewDirection, modelNormal.xyz) * rimSoftLightColor.xyz;
#endif
	
#if defined (BACK_LIGHTING)
			lightDiffuseColor += (saturate(-lightAngle) * backLightColor.xyz) * nsLightColor;
#endif
			
#if defined (SPECULAR) || (defined (SPARKLE) && !defined(SNOW))
			lightsSpecularColor += GetLightSpecularInput(input, normalizedLightDirection, viewDirection, modelNormal.xyz, lightColor, shininess, uv) * intensityMultiplier.xxx;
#endif
			
			lightsDiffuseColor += lightDiffuseColor * intensityMultiplier.xxx;
#endif
        }
    }
	
#if defined (PBR)
	//float3 ambientColor = 0.03 * baseColor.xyz * ao;
    float3 ambientColor = (mul(DirectionalAmbient, modelNormal) + IBLParams.yzw * IBLParams.xxx) * baseColor.xyz * ao;
    float3 color = ambientColor + totalRadiance;
	
	//color = color / (color + 1.0);
	//color = pow(color, 1.0 / 2.2);
#else
	
	diffuseColor += lightsDiffuseColor;
	specularColor += lightsSpecularColor;
	
#if defined (CHARACTER_LIGHT)
	float charLightMul =
		saturate(dot(viewDirection, modelNormal.xyz)) * CharacterLightParams.x +
		CharacterLightParams.y * saturate(dot(float2(0.164398998, -0.986393988), modelNormal.yz));
	float charLightColor = min(CharacterLightParams.w, max(0, CharacterLightParams.z * TexCharacterLightSampler.Sample(SampCharacterLightSampler, baseShadowUV).x));
	diffuseColor += (charLightMul * charLightColor).xxx;
#endif

#if defined(EYE)
	modelNormal.xyz = input.EyeNormal;
#endif

#if defined (ENVMAP) || defined (MULTI_LAYER_PARALLAX) || defined(EYE)
	float envMaskColor = TexEnvMaskSampler.Sample(SampEnvMaskSampler, uv).x;
	float envMask = (EnvmapData.y * (envMaskColor - glossiness) + glossiness) * (EnvmapData.x * MaterialData.x);
	float viewNormalAngle = dot(modelNormal.xyz, viewDirection);
	float3 envSamplingPoint = (viewNormalAngle * 2) * modelNormal.xyz - viewDirection;
	float3 envColor = TexEnvSampler.Sample(SampEnvSampler, envSamplingPoint).xyz * envMask.xxx;
#endif
	
	float3 emitColor = EmitColor;
#if defined (GLOWMAP)
	float3 glowColor = TexGlowSampler.Sample(SampGlowSampler, uv).xyz;
	emitColor *= glowColor;
#endif
	
	float3 directionalAmbientColor = mul(DirectionalAmbient, modelNormal);
	diffuseColor = directionalAmbientColor + emitColor.xyz + diffuseColor;
	diffuseColor += IBLParams.yzw * IBLParams.xxx;
	
	float4 color;
	color.xyz = diffuseColor * baseColor.xyz;

#endif

#if defined(HAIR)
	float3 vertexColor = (input.Color.yyy * (TintColor.xyz - 1.0.xxx) + 1.0.xxx) * color.xyz;
#else
    float3 vertexColor = input.Color.xyz * color.xyz;
#endif	

#if defined (MULTI_LAYER_PARALLAX)
	float layerValue = MultiLayerParallaxData.x * TexLayerSampler.Sample(SampLayerSampler, uv).w;
	float3 tangentViewDirection = mul(viewDirection, tbn);
	float3 layerNormal = MultiLayerParallaxData.yyy * (normalColor.xyz * 2.0.xxx + float3(-1, -1, -2)) + float3(0, 0, 1);
	float layerViewAngle = dot(-tangentViewDirection.xyz, layerNormal.xyz) * 2;
	float3 layerViewProjection = -layerNormal.xyz * layerViewAngle.xxx - tangentViewDirection.xyz;
	float2 layerUv = uv * MultiLayerParallaxData.zw + (0.0009765625 * (layerValue / abs(layerViewProjection.z))).xx * layerViewProjection.xy;
	
	float3 layerColor = TexLayerSampler.Sample(SampLayerSampler, layerUv).xyz;
	vertexColor = (saturate(viewNormalAngle) * (1 - baseColor.w)).xxx * ((directionalAmbientColor + lightsDiffuseColor) * (input.Color.xyz * layerColor) - vertexColor) + vertexColor;
#endif
	
    float4 screenPosition = mul(ScreenProj, input.WorldPosition);
    screenPosition.xy = screenPosition.xy / screenPosition.ww;
    float4 previousScreenPosition = mul(PreviousScreenProj, input.PreviousWorldPosition);
    previousScreenPosition.xy = previousScreenPosition.xy / previousScreenPosition.ww;
    float2 screenMotionVector = float2(-0.5, 0.5) * (screenPosition.xy - previousScreenPosition.xy);
	
#if !defined(PBR)
#if defined (SPECULAR)
	specularColor = (specularColor * glossiness * MaterialData.yyy) * SpecularColor.xyz;
#elif defined(SPARKLE)
	specularColor *= glossiness;
#endif
#endif
	if (useSnowSpecular)
	{
		specularColor = 0;
	}
	
#if defined (AMBIENT_SPECULAR)
	float viewAngle = saturate(dot(modelNormal.xyz, viewDirection));
	float ambientSpecularColorMultiplier = exp2(AmbientSpecularTintAndFresnelPower.w * log2(1 - viewAngle));
	float3 ambientSpecularColor = AmbientSpecularTintAndFresnelPower.xyz * saturate(mul(DirectionalAmbient, float4(modelNormal.xyz, 0.15)));
	specularColor += ambientSpecularColor * ambientSpecularColorMultiplier.xxx;
#endif
	
#if !defined(PBR) && (defined (ENVMAP) || defined (MULTI_LAYER_PARALLAX) || defined(EYE))
	vertexColor += diffuseColor * envColor;
#endif
	
    color.xyz = lerp(vertexColor.xyz, input.FogParam.xyz, input.FogParam.w);
    color.xyz = vertexColor.xyz - color.xyz * FogColor.w;
	
    float3 tmpColor = color.xyz * UnknownPerFrame2[22].yyy;
    color.xyz = tmpColor.xyz + ColourOutputClamp.xxx;
    color.xyz = min(vertexColor.xyz, color.xyz);
	
#if !defined(PBR)
	color.xyz += specularColor;
	
#if defined (SPECULAR) || defined(AMBIENT_SPECULAR)	|| defined(SPARKLE)
	float3 specularTmp = lerp(color.xyz, input.FogParam.xyz, input.FogParam.w);
	specularTmp = color.xyz - specularTmp.xyz * FogColor.w;
	
	tmpColor = specularTmp.xyz * UnknownPerFrame2[22].yyy;
	specularTmp.xyz = tmpColor.xyz + ColourOutputClamp.zzz;
	color.xyz = min(specularTmp.xyz, color.xyz);
#endif

#endif
	
#if defined (LANDSCAPE) && !defined(LOD_LAND_BLEND)
	psout.Albedo.w = 0;
#else
    float alpha = baseColor.w;
#if !defined(ADDITIONAL_ALPHA_MASK)
    alpha *= MaterialData.z;
#else
	uint2 alphaMask = input.Position.xy;
	alphaMask.x = ((alphaMask.x << 2) & 12);
	alphaMask.x = (alphaMask.y & 3) | (alphaMask.x & ~3);
	const float maskValues[16] =
	{
		0.003922,
		0.533333,
		0.133333,
		0.666667,
		0.800000,
		0.266667,
		0.933333,
		0.400000,
		0.200000,
		0.733333,
		0.066667,
		0.600000,
		0.996078,
		0.466667,
		0.866667,
		0.333333,
	};

	float testTmp = 0;
	if (MaterialData.z - maskValues[alphaMask.x] < 0)
	{
		discard;
	}
#endif
#if !(defined(TREE_ANIM) || defined(LODOBJECTSHD) || defined(LODOBJECTS))
	alpha *= input.Color.w;
#endif
#if defined (DO_ALPHA_TEST)
#if defined (DEPTH_WRITE_DECALS)
	if (alpha - 0.0156862754 < 0)
	{
		discard;
	}
	alpha = saturate(1.05 * alpha);
#endif
	if (alpha - AlphaThreshold < 0)
	{
		discard;
	}
#endif	
    psout.Albedo.w = alpha;
	
#endif
	
    psout.Albedo.xyz = color.xyz - tmpColor.xyz * UnknownPerFrame2[22].zzz;

#if defined (SNOW)
	psout.SnowParameters.x = dot(lightsSpecularColor, float3(0.3, 0.59, 0.11));
#endif
	
    psout.MotionVectors.xy = SSRParams.z > 1e-5 ? float2(1, 0) : screenMotionVector.xy;
    psout.MotionVectors.zw = float2(0, 1);
	
    float3 screenSpaceNormal;
    screenSpaceNormal.x = dot(input.ScreenNormalTransform0.xyz, normal.xyz);
    screenSpaceNormal.y = dot(input.ScreenNormalTransform1.xyz, normal.xyz);
    screenSpaceNormal.z = dot(input.ScreenNormalTransform2.xyz, normal.xyz);
    screenSpaceNormal = normalize(screenSpaceNormal);
	
    float tmp = -1e-5 + SSRParams.x;
	float tmp3 = (SSRParams.y - tmp);
	float tmp2 = (glossiness - tmp);
	float tmp1 = 1 / tmp3;
	tmp = saturate(tmp1 * tmp2);
    tmp *= tmp * (3 + -2 * tmp);
    psout.ScreenSpaceNormals.w = tmp * SSRParams.w;
	
    screenSpaceNormal.z = max(0.001, sqrt(8 + -8 * screenSpaceNormal.z));
    screenSpaceNormal.xy /= screenSpaceNormal.zz;
    psout.ScreenSpaceNormals.xy = screenSpaceNormal.xy + 0.5.xx;
    psout.ScreenSpaceNormals.z = 0;

#if defined(OUTLINE)
	psout.Albedo = float4(1, 0, 0, 1);
#endif
	
    return psout;
}
#endif
