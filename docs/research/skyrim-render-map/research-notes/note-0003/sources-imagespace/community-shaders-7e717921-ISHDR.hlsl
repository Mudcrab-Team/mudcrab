#include "Common/Color.hlsli"
#include "Common/DummyVSTexCoord.hlsl"
#include "Common/FrameBuffer.hlsli"

typedef VS_OUTPUT PS_INPUT;

struct PS_OUTPUT
{
	float4 Color : SV_Target0;
};

#if defined(PSHADER)
SamplerState ImageSampler : register(s0);
#	if defined(DOWNSAMPLE)
SamplerState AdaptSampler : register(s1);
#	elif defined(TONEMAP)
SamplerState BlendSampler : register(s1);
#	endif
SamplerState AvgSampler : register(s2);

Texture2D<float4> ImageTex : register(t0);
#	if defined(DOWNSAMPLE)
Texture2D<float4> AdaptTex : register(t1);
#	elif defined(TONEMAP)
Texture2D<float4> BlendTex : register(t1);
#	endif
Texture2D<float4> AvgTex : register(t2);

cbuffer PerGeometry : register(b2)
{
	float4 Flags : packoffset(c0);
	float4 TimingData : packoffset(c1);
	float4 Param : packoffset(c2);
	float4 Cinematic : packoffset(c3);
	float4 Tint : packoffset(c4);
	float4 Fade : packoffset(c5);
	float4 BlurScale : packoffset(c6);
	float4 BlurOffsets[16] : packoffset(c7);
};

float GetTonemapFactorReinhard(float luminance)
{
	return (luminance * (luminance * Param.y + 1)) / (luminance + 1);
}

float GetTonemapFactorHejlBurgessDawson(float luminance)
{
	float tmp = max(0, luminance - 0.004);
	return Param.y *
	       pow(((tmp * 6.2 + 0.5) * tmp) / (tmp * (tmp * 6.2 + 1.7) + 0.06), GammaCorrectionValue);
}

PS_OUTPUT main(PS_INPUT input)
{
	PS_OUTPUT psout;

#	if defined(DOWNSAMPLE)
	float3 downsampledColor = 0;
	for (int sampleIndex = 0; sampleIndex < SAMPLES_COUNT; ++sampleIndex) {
		float2 texCoord = BlurOffsets[sampleIndex].xy * BlurScale.xy + input.TexCoord;
		if (Flags.x > 0.5) {
			texCoord = GetDynamicResolutionAdjustedScreenPosition(texCoord);
		}
		float3 imageColor = ImageTex.Sample(ImageSampler, texCoord).xyz;
#		if defined(LUM)
		imageColor = imageColor.x;
#		elif defined(RGB2LUM)
		imageColor = RGBToLuminance(imageColor);
#		endif
		downsampledColor += imageColor * BlurOffsets[sampleIndex].z;
	}
#		if defined(LIGHT_ADAPT)
	float2 adaptValue = AdaptTex.Sample(AdaptSampler, input.TexCoord).xy;
	if (isnan(downsampledColor.x) || isnan(downsampledColor.y) || isnan(downsampledColor.z)) {
		downsampledColor.xy = adaptValue;
	} else {
		float2 adaptDelta = downsampledColor.xy - adaptValue;
		downsampledColor.xy =
			sign(adaptDelta) * clamp(abs(Param.wz * adaptDelta), 0.00390625, abs(adaptDelta)) +
			adaptValue;
	}
#		endif
	psout.Color = float4(downsampledColor, BlurScale.z);

#	elif defined(TONEMAP)
	float2 adjustedTexCoord = GetDynamicResolutionAdjustedScreenPosition(input.TexCoord);
	float3 blendColor = BlendTex.Sample(BlendSampler, adjustedTexCoord).xyz;
	float3 imageColor = 0;
	if (Flags.x > 0.5) {
		imageColor = ImageTex.Sample(ImageSampler, adjustedTexCoord).xyz;
	} else {
		imageColor = ImageTex.Sample(ImageSampler, input.TexCoord).xyz;
	}
	float2 avgValue = AvgTex.Sample(AvgSampler, input.TexCoord).xy;

	float luminance = max(1e-5, RGBToLuminance(blendColor));
	float exposureAdjustedLuminance = (avgValue.y / avgValue.x) * luminance;
	float blendFactor;
	if (Param.z > 0.5) {
		blendFactor = GetTonemapFactorHejlBurgessDawson(exposureAdjustedLuminance);
	} else {
		blendFactor = GetTonemapFactorReinhard(exposureAdjustedLuminance);
	}

	float3 blendedColor =
		blendColor * (blendFactor / luminance) + saturate(Param.x - blendFactor) * imageColor;
	float blendedLuminance = RGBToLuminance(blendedColor);

	float4 linearColor = lerp(avgValue.x,
		Cinematic.w * lerp(lerp(blendedLuminance, float4(blendedColor, 1), Cinematic.x),
						  blendedLuminance * Tint, Tint.w),
		Cinematic.z);
	float4 srgbColor = float4(ToSRGBColor(saturate(linearColor.xyz)), linearColor.w);
#		if defined(FADE)
	srgbColor = lerp(srgbColor, Fade, Fade.w);
#		endif
	psout.Color = srgbColor;

#	endif

	return psout;
}
#endif
