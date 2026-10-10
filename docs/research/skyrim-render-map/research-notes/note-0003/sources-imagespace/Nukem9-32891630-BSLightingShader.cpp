#include "../../../rendering/common.h"
#include "../../../../common.h"
#include "../../NiMain/NiSourceTexture.h"
#include "../../BSGraphics/BSGraphicsUtility.h"
#include "../../TES.h"
#include "../../Setting.h"
#include "../../NiMain/NiDirectionalLight.h"
#include "../../NiMain/BSMultiIndexTriShape.h"
#include "../../NiMain/NiFogProperty.h"
#include "../BSShaderManager.h"
#include "../BSShaderUtil.h"
#include "../BSLight.h"
#include "../BSShadowLight.h"
#include "BSLightingShader.h"
#include "BSLightingShaderProperty.h"
#include "BSLightingShaderMaterial.h"

DEFINE_SHADER_DESCRIPTOR(
	Lighting,

	// Vertex
	CONFIG_ENTRY(VS, PER_TEC, 12, float4, HighDetailRange)
	CONFIG_ENTRY(VS, PER_TEC, 13, float4, FogParam)
	CONFIG_ENTRY(VS, PER_TEC, 14, float4, FogNearColor)
	CONFIG_ENTRY(VS, PER_TEC, 15, float4, FogFarColor)

	CONFIG_ENTRY(VS, PER_MAT, 9, float4, LeftEyeCenter)
	CONFIG_ENTRY(VS, PER_MAT, 10, float4, RightEyeCenter)
	CONFIG_ENTRY(VS, PER_MAT, 11, float4, TexcoordOffset)

	CONFIG_ENTRY(VS, PER_GEO, 0, row_major float3x4, World)
	CONFIG_ENTRY(VS, PER_GEO, 1, row_major float3x4, PreviousWorld)
	CONFIG_ENTRY(VS, PER_GEO, 2, float3, EyePosition)
	CONFIG_ENTRY(VS, PER_GEO, 3, float4, LandBlendParams)
	CONFIG_ENTRY(VS, PER_GEO, 4, float4, TreeParams)
	CONFIG_ENTRY(VS, PER_GEO, 5, float2, WindTimers)
	CONFIG_ENTRY(VS, PER_GEO, 6, row_major float3x4, TextureProj)
	CONFIG_ENTRY(VS, PER_GEO, 7, float, IndexScale)
	CONFIG_ENTRY(VS, PER_GEO, 8, float4, WorldMapOverlayParameters)
	CONFIG_ENTRY(VS, PER_GEO, 16, unknown, Bones)								// Unknown, not found anywhere

	// Pixel
	CONFIG_ENTRY(PS, PER_TEC, 11, float4, VPOSOffset)							// Only found in shaders with RAW_FLAG_DEFSHADOW
	CONFIG_ENTRY(PS, PER_TEC, 19, float4, FogColor)
	CONFIG_ENTRY(PS, PER_TEC, 20, float4, ColourOutputClamp)

	CONFIG_ENTRY(PS, PER_MAT, 21, float4, EnvmapData)
	CONFIG_ENTRY(PS, PER_MAT, 22, float4, ParallaxOccData)
	CONFIG_ENTRY(PS, PER_MAT, 23, float4, TintColor)
	CONFIG_ENTRY(PS, PER_MAT, 24, float4, LODTexParams)
	CONFIG_ENTRY(PS, PER_MAT, 25, float4, SpecularColor)
	CONFIG_ENTRY(PS, PER_MAT, 26, float4, SparkleParams)
	CONFIG_ENTRY(PS, PER_MAT, 27, float4, MultiLayerParallaxData)
	CONFIG_ENTRY(PS, PER_MAT, 28, float4, LightingEffectParams)
	CONFIG_ENTRY(PS, PER_MAT, 29, float4, IBLParams)
	CONFIG_ENTRY(PS, PER_MAT, 30, float4, LandscapeTexture1to4IsSnow)
	CONFIG_ENTRY(PS, PER_MAT, 31, float4, LandscapeTexture5to6IsSnow)
	CONFIG_ENTRY(PS, PER_MAT, 32, float4, LandscapeTexture1to4IsSpecPower)
	CONFIG_ENTRY(PS, PER_MAT, 33, float4, LandscapeTexture5to6IsSpecPower)
	CONFIG_ENTRY(PS, PER_MAT, 34, float4, SnowRimLightParameters)
	CONFIG_ENTRY(PS, PER_MAT, 35, float4, CharacterLightParams)

	CONFIG_ENTRY(PS, PER_GEO, 0, float2, NumLightNumShadowLight)
	CONFIG_ENTRY(PS, PER_GEO, 1, float4[7], PointLightPosition)
	CONFIG_ENTRY(PS, PER_GEO, 2, float4[7], PointLightColor)
	CONFIG_ENTRY(PS, PER_GEO, 3, float3, DirLightDirection)
	CONFIG_ENTRY(PS, PER_GEO, 4, float3, DirLightColor)
	CONFIG_ENTRY(PS, PER_GEO, 5, row_major float3x4, DirectionalAmbient)
	CONFIG_ENTRY(PS, PER_GEO, 6, float4, AmbientSpecularTintAndFresnelPower)	// BUG: Vanilla code incorrectly labels this as PerMaterial
	CONFIG_ENTRY(PS, PER_GEO, 7, float4, MaterialData)
	CONFIG_ENTRY(PS, PER_GEO, 8, float3, EmitColor)
	// CONFIG_ENTRY(PS, PER_GEO, X, float3, GlitterParams)						// BUG: Unused, undocumented, type assumed from PS4, shader cbuffer offset 0x50
	CONFIG_ENTRY(PS, PER_GEO, 9, float, AlphaTestRef)							// Unused, type assumed from PS4
	CONFIG_ENTRY(PS, PER_GEO, 10, float4, ShadowLightMaskSelect)
	CONFIG_ENTRY(PS, PER_GEO, 12, float4, ProjectedUVParams)
	CONFIG_ENTRY(PS, PER_GEO, 13, float4, ProjectedUVParams2)
	CONFIG_ENTRY(PS, PER_GEO, 14, float4, ProjectedUVParams3)
	CONFIG_ENTRY(PS, PER_GEO, 15, unknown, SplitDistance)						// Unknown, not found anywhere
	CONFIG_ENTRY(PS, PER_GEO, 16, float4, SSRParams)
	CONFIG_ENTRY(PS, PER_GEO, 17, float4, WorldMapOverlayParametersPS)
	CONFIG_ENTRY(PS, PER_GEO, 18, unknown, AmbientColor)						// Unknown, not found anywhere
);

//
// Shader notes:
//
// - Constructor is not implemented
// - Destructor is not implemented
// - Vanilla bug fix for SetupMaterial() case RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW
// - Global variables eliminated in each Setup/Restore function
// - A lock is held in GeometrySetupConstantLandBlendParams()
// - "Bones" and "IndexScale" vertex constants are not used in the game (undefined types)
// - AmbientSpecularTintAndFresnelPower has a bug where it's set in SetupMaterial() rather than SetupGeometry()
// - An unknown/redundant global variable edit was removed in GeometrySetupConstantLandBlendParams (BSShaderManager::St.kOldGridArrayCenter)
//
using namespace DirectX;
using namespace BSGraphics;

AutoPtr(NiSourceTexture *, WorldMapOverlayNormalTexture, 0x1E32F90);
AutoPtr(NiSourceTexture *, WorldMapOverlayNormalSnowTexture, 0x1E32F98);
AutoPtr(int, dword_143051B3C, 0x3051B3C);
AutoPtr(int, dword_143051B40, 0x3051B40);
AutoPtr(uintptr_t, qword_14304F260, 0x304F260);
AutoPtr(float, flt_143257C50, 0x3257C50);// fLightingOutputColourClampPostLit_General
AutoPtr(float, flt_143257C54, 0x3257C54);// fLightingOutputColourClampPostEnv_General
AutoPtr(float, flt_143257C58, 0x3257C58);// fLightingOutputColourClampPostSpec_General
AutoPtr(int, dword_141E33BA0, 0x1E33BA0);
AutoPtr(uintptr_t, qword_1431F5810, 0x31F5810);// ImagespaceShaderManager
AutoPtr(XMVECTORF32, xmmword_14187D940, 0x187D940);
AutoPtr(BYTE, byte_141E32E88, 0x1E32E88);
AutoPtr(float, flt_143257C40, 0x3257C40);
//AutoPtr(uint32_t, dword_141E3527C, 0x1E3527C);// Replaced by TLS
//AutoPtr(uint32_t, dword_141E35280, 0x1E35280);// Replaced by TLS
AutoPtr(NiColorA, dword_1431F5540, 0x31F5540);// Unknown setting from SceneGraph
AutoPtr(NiColorA, dword_1431F5550, 0x31F5550);// 4x fMapMenuOverlayScale settings
//AutoPtr(float, xmmword_141880020, 0x1880020);
AutoPtr(bool, byte_1431F547C, 0x31F547C);// BSShaderManager::bLODFadeInProgress
AutoPtr(XMFLOAT4, xmmword_141E32FC8, 0x1E32FC8);
AutoPtr(BSFadeNode *, qword_1431F5410, 0x31F5410);

DefineIniSetting(bEnableSnowMask, Display);
DefineIniSetting(iLandscapeMultiNormalTilingFactor, Display);
DefineIniSetting(fSnowRimLightIntensity, Display);
DefineIniSetting(fSnowGeometrySpecPower, Display);
DefineIniSetting(fSnowNormalSpecPower, Display);
DefineIniSetting(bEnableSnowRimLighting, Display);
DefineIniSetting(fSpecMaskBegin, Display);
DefineIniSetting(fSpecMaskSpan, Display);
DefineIniSetting(bEnableProjecteUVDiffuseNormals, Display);
DefineIniSetting(bEnableProjecteUVDiffuseNormalsOnCubemap, Display);
DefineIniSetting(fProjectedUVDiffuseNormalTilingScale, Display);
DefineIniSetting(fProjectedUVNormalDetailTilingScale, Display);
DefineIniSetting(bEnableParallaxOcclusion, Display);
DefineIniSetting(iShadowMaskQuarter, Display);

thread_local uint32_t TLS_m_CurrentRawTechnique;
thread_local DepthStencilDepthMode TLS_dword_141E35280;
thread_local uint32_t TLS_dword_141E3527C;

char hookbuffer[50];

void TestHook5()
{
	memcpy(hookbuffer, (PBYTE)(g_ModuleBase + 0x1307BD0), 50);

	Detours::X64::DetourFunctionClass(g_ModuleBase + 0x1307BD0, &BSLightingShader::__ctor__);
}

BSLightingShader::BSLightingShader() : BSShader(ShaderConfigLighting.Type)
{
	uintptr_t v1 = *(uintptr_t *)((uintptr_t)this + 0x0);
	uintptr_t v2 = *(uintptr_t *)((uintptr_t)this + 0x10);
	uintptr_t v3 = *(uintptr_t *)((uintptr_t)this + 0x18);

	XUtil::PatchMemory((g_ModuleBase + 0x1307BD0), (PBYTE)&hookbuffer, 50);

	AutoFunc(uintptr_t(__fastcall *)(BSLightingShader *), sub_141307BD0, 0x1307BD0);
	sub_141307BD0(this);

	ShaderMetadata[BSShaderManager::BSSM_SHADER_LIGHTING] = &ShaderConfigLighting;
	m_Type = BSShaderManager::BSSM_SHADER_LIGHTING;
	pInstance = this;

	*(uintptr_t *)((uintptr_t)this + 0x0) = v1;
	*(uintptr_t *)((uintptr_t)this + 0x10) = v2;
	*(uintptr_t *)((uintptr_t)this + 0x18) = v3;
}

BSLightingShader::~BSLightingShader()
{
	Assert(false);
}

extern D3D_PRIMITIVE_TOPOLOGY TopoOverride;

bool BSLightingShader::SetupTechnique(uint32_t Technique)
{
	BSSHADER_FORWARD_CALL(TECHNIQUE, &BSLightingShader::SetupTechnique, Technique);

	// Check if shaders exist
	uint32_t rawTechnique = GetRawTechnique(Technique);

	if (!BeginTechnique(GetVertexTechnique(rawTechnique), GetPixelTechnique(rawTechnique), false))
		return false;

	//m_CurrentRawTechnique = rawTechnique;
	TLS_m_CurrentRawTechnique = rawTechnique;

	auto renderer = BSGraphics::Renderer::QInstance();
	auto state = renderer->GetRendererShadowState();

	auto vertexCG = renderer->GetShaderConstantGroup(state->m_CurrentVertexShader, BSGraphics::CONSTANT_GROUP_LEVEL_TECHNIQUE);
	auto pixelCG = renderer->GetShaderConstantGroup(state->m_CurrentPixelShader, BSGraphics::CONSTANT_GROUP_LEVEL_TECHNIQUE);

	renderer->SetTextureFilterMode(0, 3);
	renderer->SetTextureFilterMode(1, 3);

	switch ((rawTechnique >> 24) & 0x3F)
	{
	case RAW_TECHNIQUE_ENVMAP:
	case RAW_TECHNIQUE_EYE:
		renderer->SetTextureFilterMode(4, 3);
		renderer->SetTextureFilterMode(5, 3);
		break;

	case RAW_TECHNIQUE_GLOWMAP:
		renderer->SetTextureFilterMode(6, 3);
		break;

	case RAW_TECHNIQUE_PARALLAX:
	case RAW_TECHNIQUE_PARALLAXOCC:
		renderer->SetTextureFilterMode(3, 3);
		break;

	case RAW_TECHNIQUE_FACEGEN:
		renderer->SetTextureFilterMode(3, 3);
		renderer->SetTextureFilterMode(4, 3);
		renderer->SetTextureFilterMode(12, 3);
		break;

	case RAW_TECHNIQUE_MTLAND:
	case RAW_TECHNIQUE_MTLANDLODBLEND:
	{
		TopoOverride = D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST;
		// Override all 16 samplers
		for (int i = 0; i < 16; i++)
		{
			renderer->SetTexture(i, BSGraphics::gState.pDefaultTextureBlack);
			renderer->SetTextureMode(i, 3, 3);
		}

		renderer->SetTextureMode(15, 3, 1);
	}
	break;

	case RAW_TECHNIQUE_LODLAND:
	case RAW_TECHNIQUE_LODLANDNOISE:
		TopoOverride = D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST;
		renderer->SetTextureMode(0, 3, 1);
		renderer->SetTextureMode(1, 3, 1);
		TechUpdateHighDetailRangeConstants(vertexCG);
		break;

	case RAW_TECHNIQUE_MULTILAYERPARALLAX:
		renderer->SetTextureFilterMode(4, 3);
		renderer->SetTextureFilterMode(5, 3);
		renderer->SetTextureFilterMode(8, 3);
		break;

	case RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW:
		renderer->SetTexture(10, BSGraphics::gState.pDefaultTextureProjNormalMap);
		renderer->SetTextureMode(10, 3, 0);
		break;
	}

	TechUpdateFogConstants(vertexCG, pixelCG);

	// PS: p20 float4 ColourOutputClamp
	{
		XMVECTORF32& colourOutputClamp = pixelCG.ParamPS<XMVECTORF32, 20>();

		colourOutputClamp.f[0] = flt_143257C50;// Initial value is fLightingOutputColourClampPostLit
		colourOutputClamp.f[1] = flt_143257C54;// Initial value is fLightingOutputColourClampPostEnv
		colourOutputClamp.f[2] = flt_143257C58;// Initial value is fLightingOutputColourClampPostSpec
		colourOutputClamp.f[3] = 0.0f;
	}

	bool shadowed = (rawTechnique & RAW_FLAG_SHADOW_DIR) || (rawTechnique & (RAW_FLAG_LIGHTCOUNT6 | RAW_FLAG_LIGHTCOUNT5 | RAW_FLAG_LIGHTCOUNT4));
	bool defShadow = (rawTechnique & RAW_FLAG_DEFSHADOW);

	// NOTE: A use-after-free has been eliminated. Constants are flushed AFTER this code block now.
	if (shadowed && defShadow)
	{
		renderer->SetShaderResource(14, (ID3D11ShaderResourceView *)qword_14304F260);
		renderer->SetTextureMode(14, 0, (iShadowMaskQuarter->uValue.i != 4) ? 1 : 0);

		// PS: p11 float4 VPOSOffset
		XMVECTORF32& vposOffset = pixelCG.ParamPS<XMVECTORF32, 11>();

		vposOffset.f[0] = 1.0f / (float)dword_143051B3C;
		vposOffset.f[1] = 1.0f / (float)dword_143051B40;
		vposOffset.f[2] = 0.0f;
		vposOffset.f[3] = 0.0f;
	}

	ID3D11Buffer *cbuffer12;
	renderer->Data.pContext->PSGetConstantBuffers(12, 1, &cbuffer12);
	renderer->Data.pContext->DSSetConstantBuffers(12, 1, &cbuffer12);
	cbuffer12->Release();

	renderer->FlushConstantGroupVSPS(&vertexCG, &pixelCG);
	renderer->ApplyConstantGroupVSPS(&vertexCG, &pixelCG, BSGraphics::CONSTANT_GROUP_LEVEL_TECHNIQUE);
	return true;
}

void BSLightingShader::RestoreTechnique(uint32_t Technique)
{
	BSSHADER_FORWARD_CALL(TECHNIQUE, &BSLightingShader::RestoreTechnique, Technique);

	auto renderer = BSGraphics::Renderer::QInstance();

	if (TLS_m_CurrentRawTechnique & RAW_FLAG_DEFSHADOW)
		renderer->SetShaderResource(14, nullptr);

	uintptr_t v2 = *(uintptr_t *)(qword_1431F5810 + 448);

	if (v2 && *(bool *)(v2 + 16) && *(bool *)(v2 + 17))
		renderer->SetShaderResource(15, nullptr);

	TopoOverride = D3D11_PRIMITIVE_TOPOLOGY_UNDEFINED;
	EndTechnique();
}

void BSLightingShader::SetupMaterial(BSShaderMaterial const *Material)
{
	BSSHADER_FORWARD_CALL(MATERIAL, &BSLightingShader::SetupMaterial, Material);

	auto renderer = BSGraphics::Renderer::QInstance();
	auto state = renderer->GetRendererShadowState();

	auto vertexCG = renderer->GetShaderConstantGroup(state->m_CurrentVertexShader, BSGraphics::CONSTANT_GROUP_LEVEL_MATERIAL);
	auto pixelCG = renderer->GetShaderConstantGroup(state->m_CurrentPixelShader, BSGraphics::CONSTANT_GROUP_LEVEL_MATERIAL);

	const uint32_t rawTechnique = TLS_m_CurrentRawTechnique;
	const uint32_t baseTechniqueID = (rawTechnique >> 24) & 0x3F;
	bool setDiffuseNormalSamplers = true;

	const uintptr_t v5 = (uintptr_t)this;
	const BSLightingShaderMaterialBase *lightingMaterial = static_cast<const BSLightingShaderMaterialBase *>(Material);

	switch (baseTechniqueID)
	{
	case RAW_TECHNIQUE_ENVMAP:
	{
		auto m = static_cast<const BSLightingShaderMaterialEnvmap *>(lightingMaterial);

		MatSetEnvTexture(m->spEnvTexture, m);
		MatSetEnvMaskTexture(m->spEnvMaskTexture, m);

		// PS: p21 float4 EnvmapData
		XMVECTORF32& envmapData = pixelCG.ParamPS<XMVECTORF32, 21>();

		envmapData.f[0] = m->fEnvmapScale;
		envmapData.f[1] = m->spEnvMaskTexture ? 1.0f : 0.0f;
	}
	break;

	case RAW_TECHNIQUE_GLOWMAP:
	{
		auto m = static_cast<const BSLightingShaderMaterialGlowmap *>(lightingMaterial);

		MatSetTextureSlot(6, m->spGlowTexture, m);
	}
	break;

	case RAW_TECHNIQUE_PARALLAX:
	{
		auto m = static_cast<const BSLightingShaderMaterialParallax *>(lightingMaterial);

		MatSetTextureSlot(3, m->spHeightTexture, m);
	}
	break;

	case RAW_TECHNIQUE_FACEGEN:
	{
		auto m = static_cast<const BSLightingShaderMaterialFacegen *>(lightingMaterial);

		MatSetTextureSlot(3, m->spTintTexture, m);
		MatSetTextureSlot(4, m->spDetailTexture, m);
		MatSetTextureSlot(12, m->spSubsurfaceTexture, m);
	}
	break;

	case RAW_TECHNIQUE_FACEGENRGBTINT:
	{
		auto m = static_cast<const BSLightingShaderMaterialFacegenTint *>(lightingMaterial);

		// PS: p23 float4 TintColor
		XMVECTORF32& tintColor = pixelCG.ParamPS<XMVECTORF32, 23>();

		tintColor.f[0] = m->kTintColor.r;
		tintColor.f[1] = m->kTintColor.g;
		tintColor.f[2] = m->kTintColor.b;
	}
	break;

	case RAW_TECHNIQUE_HAIR:
	{
		auto m = static_cast<const BSLightingShaderMaterialHairTint *>(lightingMaterial);

		// PS: p23 float4 TintColor
		XMVECTORF32& tintColor = pixelCG.ParamPS<XMVECTORF32, 23>();

		tintColor.f[0] = m->kHairTintColor.r;
		tintColor.f[1] = m->kHairTintColor.g;
		tintColor.f[2] = m->kHairTintColor.b;
	}
	break;

	case RAW_TECHNIQUE_PARALLAXOCC:
	{
		auto m = static_cast<const BSLightingShaderMaterialParallaxOcc *>(lightingMaterial);

		MatSetTextureSlot(3, m->spHeightTexture, m);

		// PS: p22 float4 ParallaxOccData
		XMVECTORF32& parallaxOccData = pixelCG.ParamPS<XMVECTORF32, 22>();

		parallaxOccData.f[0] = m->fParallaxOccScale;	// Reversed on purpose?
		parallaxOccData.f[1] = m->fParallaxOccMaxPasses;// Reversed on purpose?
	}
	break;

	case RAW_TECHNIQUE_MTLAND:
	case RAW_TECHNIQUE_MTLANDLODBLEND:
	{
		auto m = static_cast<const BSLightingShaderMaterialLandscape *>(lightingMaterial);

		// PS: p24 float4 LODTexParams
		{
			XMVECTORF32& lodTexParams = pixelCG.ParamPS<XMVECTORF32, 24>();

			lodTexParams.f[0] = m->fTerrainTexOffsetX;
			lodTexParams.f[1] = m->fTerrainTexOffsetY;
			lodTexParams.f[2] = byte_141E32E88 ? 1.0f : 0.0f;
			lodTexParams.f[3] = m->fTerrainTexFade;
		}

		MatSetMultiTextureLandOverrides(m);

		if (m->spTerrainNoiseTexture)
			MatSetTextureSlot(15, m->spTerrainNoiseTexture, m);

		setDiffuseNormalSamplers = false;

		// PS: p32 float4 LandscapeTexture1to4IsSpecPower
		{
			XMVECTORF32& landscapeTexture1to4IsSpecPower = pixelCG.ParamPS<XMVECTORF32, 32>();

			landscapeTexture1to4IsSpecPower.f[0] = m->fTextureIsSpecPower[0];
			landscapeTexture1to4IsSpecPower.f[1] = m->fTextureIsSpecPower[1];
			landscapeTexture1to4IsSpecPower.f[2] = m->fTextureIsSpecPower[2];
			landscapeTexture1to4IsSpecPower.f[3] = m->fTextureIsSpecPower[3];
		}

		// PS: p33 float4 LandscapeTexture5to6IsSpecPower
		{
			XMVECTORF32& landscapeTexture5to6IsSpecPower = pixelCG.ParamPS<XMVECTORF32, 33>();

			landscapeTexture5to6IsSpecPower.f[0] = m->fTextureIsSpecPower[4];
			landscapeTexture5to6IsSpecPower.f[1] = m->fTextureIsSpecPower[5];
			landscapeTexture5to6IsSpecPower.f[2] = 0.0f;
			landscapeTexture5to6IsSpecPower.f[3] = 0.0f;
		}

		if (rawTechnique & RAW_FLAG_SNOW)
		{
			// PS: p30 float4 LandscapeTexture1to4IsSnow
			XMVECTORF32& landscapeTexture1to4IsSnow = pixelCG.ParamPS<XMVECTORF32, 30>();

			landscapeTexture1to4IsSnow.f[0] = m->fTextureIsSnow[0];
			landscapeTexture1to4IsSnow.f[1] = m->fTextureIsSnow[1];
			landscapeTexture1to4IsSnow.f[2] = m->fTextureIsSnow[2];
			landscapeTexture1to4IsSnow.f[3] = m->fTextureIsSnow[3];

			// PS: p31 float4 LandscapeTexture5to6IsSnow
			XMVECTORF32& LandscapeTexture5to6IsSnow = pixelCG.ParamPS<XMVECTORF32, 31>();

			LandscapeTexture5to6IsSnow.f[0] = m->fTextureIsSnow[4];
			LandscapeTexture5to6IsSnow.f[1] = m->fTextureIsSnow[5];
			LandscapeTexture5to6IsSnow.f[2] = bEnableSnowMask->uValue.b ? 1.0f : 0.0f;
			LandscapeTexture5to6IsSnow.f[3] = 1.0f / iLandscapeMultiNormalTilingFactor->uValue.i;
		}
	}
	break;

	case RAW_TECHNIQUE_LODLAND:
	case RAW_TECHNIQUE_LODLANDNOISE:
	{
		auto m = static_cast<const BSLightingShaderMaterialLODLandscape *>(lightingMaterial);

		// PS: p24 float4 LODTexParams
		{
			XMVECTORF32& lodTexParams = pixelCG.ParamPS<XMVECTORF32, 24>();

			lodTexParams.f[0] = m->fTerrainTexOffsetX;
			lodTexParams.f[1] = m->fTerrainTexOffsetY;
			lodTexParams.f[2] = byte_141E32E88 ? 1.0f : 0.0f;
			lodTexParams.f[3] = m->fTerrainTexFade;
		}

		if (baseTechniqueID == RAW_TECHNIQUE_LODLANDNOISE)
		{
			if (m->spLandscapeNoiseTexture)
				MatSetTextureSlot(15, m->spLandscapeNoiseTexture, m);

			renderer->SetTextureMode(15, 3, 1);
		}
	}
	break;

	case RAW_TECHNIQUE_MULTILAYERPARALLAX:
	{
		auto m = static_cast<const BSLightingShaderMaterialMultiLayerParallax *>(lightingMaterial);

		MatSetTextureSlot(8, m->spLayerTexture, m);

		// PS: p27 float4 MultiLayerParallaxData
		{
			XMVECTORF32& multiLayerParallaxData = pixelCG.ParamPS<XMVECTORF32, 27>();

			multiLayerParallaxData.f[0] = m->fParallaxLayerThickness;
			multiLayerParallaxData.f[1] = m->fParallaxRefractionScale;
			multiLayerParallaxData.f[2] = m->fParallaxInnerLayerUScale;
			multiLayerParallaxData.f[3] = m->fParallaxInnerLayerVScale;
		}

		MatSetEnvTexture(m->spEnvTexture, m);
		MatSetEnvMaskTexture(m->spEnvMaskTexture, m);

		// PS: p21 float4 EnvmapData
		{
			XMVECTORF32& envmapData = pixelCG.ParamPS<XMVECTORF32, 21>();

			envmapData.f[0] = m->fEnvmapScale;
			envmapData.f[1] = m->spEnvMaskTexture ? 1.0f : 0.0f;
		}
	}
	break;

	case RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW:
	{
		auto m = static_cast<const BSLightingShaderMaterialSnow *>(lightingMaterial);

		// PS: p26 float4 SparkleParams
		XMVECTORF32& sparkleParams = pixelCG.ParamPS<XMVECTORF32, 26>();

		// if (is BSLightingShaderMaterialSnow RTTI instance)
		if (*(uintptr_t *)(*(uintptr_t *)(lightingMaterial) - 8) == (g_ModuleBase + 0x19AA838))
		{
			sparkleParams.f[0] = m->kSparkleParams.r;
			sparkleParams.f[1] = m->kSparkleParams.g;
			sparkleParams.f[2] = m->kSparkleParams.b;
			sparkleParams.f[3] = m->kSparkleParams.a;
		}
		else
		{
			// Bug fix: Without the RTTI check, this would read out-of-bounds data causing a crash or
			// simply invalid values
			sparkleParams.v = _mm_set1_ps(1.0f);
		}
	}
	break;

	case RAW_TECHNIQUE_EYE:
	{
		auto m = static_cast<const BSLightingShaderMaterialEye *>(lightingMaterial);

		MatSetEnvTexture(m->spEnvTexture, m);
		MatSetEnvMaskTexture(m->spEnvMaskTexture, m);

		// VS: p9 float4 LeftEyeCenter
		{
			XMVECTORF32& leftEyeCenter = vertexCG.ParamVS<XMVECTORF32, 9>();

			leftEyeCenter.f[0] = m->kEyeCenter[0].x;
			leftEyeCenter.f[1] = m->kEyeCenter[0].y;
			leftEyeCenter.f[2] = m->kEyeCenter[0].z;
		}

		// VS: p10 float4 RightEyeCenter
		{
			XMVECTORF32& rightEyeCenter = vertexCG.ParamVS<XMVECTORF32, 10>();

			rightEyeCenter.f[0] = m->kEyeCenter[1].x;
			rightEyeCenter.f[1] = m->kEyeCenter[1].y;
			rightEyeCenter.f[2] = m->kEyeCenter[1].z;
		}

		// PS: p21 float4 EnvmapData
		{
			XMVECTORF32& envmapData = pixelCG.ParamPS<XMVECTORF32, 21>();

			envmapData.f[0] = m->fEnvmapScale;
			envmapData.f[1] = m->spEnvMaskTexture ? 1.0f : 0.0f;
		}
	}
	break;
	}

	// VS: p11 float4 TexcoordOffset
	{
		XMVECTORF32& texcoordOffset = vertexCG.ParamVS<XMVECTORF32, 11>();

		texcoordOffset.f[0] = lightingMaterial->kTexCoordOffset[BSShaderManager::St.uiTextureTransformCurrentBuffer].x;
		texcoordOffset.f[1] = lightingMaterial->kTexCoordOffset[BSShaderManager::St.uiTextureTransformCurrentBuffer].y;
		texcoordOffset.f[2] = lightingMaterial->kTexCoordScale[BSShaderManager::St.uiTextureTransformCurrentBuffer].x;
		texcoordOffset.f[3] = lightingMaterial->kTexCoordScale[BSShaderManager::St.uiTextureTransformCurrentBuffer].y;
	}

	if (rawTechnique & RAW_FLAG_SPECULAR)
	{
		// PS: p25 float4 SpecularColor
		XMVECTORF32& specularColor = pixelCG.ParamPS<XMVECTORF32, 25>();

		specularColor.f[0] = lightingMaterial->kSpecularColor.r * lightingMaterial->fSpecularColorScale;
		specularColor.f[1] = lightingMaterial->kSpecularColor.g * lightingMaterial->fSpecularColorScale;
		specularColor.f[2] = lightingMaterial->kSpecularColor.b * lightingMaterial->fSpecularColorScale;
		specularColor.f[3] = lightingMaterial->fSpecularPower;

		if (rawTechnique & RAW_FLAG_MODELSPACENORMALS)
		{
			MatSetTextureSlot(2, lightingMaterial->spSpecularBackLightingTexture, lightingMaterial);

			renderer->SetTexture(2, lightingMaterial->spSpecularBackLightingTexture);
			renderer->SetTextureMode(2, lightingMaterial->eTextureClampMode, 3);
		}
	}

	if (rawTechnique & RAW_FLAG_AMBIENT_SPECULAR)
	{
		// PS: p6 float4 AmbientSpecularTintAndFresnelPower
		BSGraphics::Utility::CopyNiColorAToFloat(&pixelCG.ParamPS<XMVECTOR, 6>(), BSShaderManager::St.AmbientSpecular);
	}

	// These two conditions were originally separate code blocks
	if ((rawTechnique & RAW_FLAG_SOFT_LIGHTING) || (rawTechnique & RAW_FLAG_RIM_LIGHTING))
	{
		renderer->SetTexture(12, lightingMaterial->spRimSoftLightingTexture);
		renderer->SetTextureAddressMode(12, lightingMaterial->eTextureClampMode);

		// PS: p28 float4 LightingEffectParams
		XMVECTORF32& lightingEffectParams = pixelCG.ParamPS<XMVECTORF32, 28>();

		lightingEffectParams.f[0] = lightingMaterial->fSubSurfaceLightRolloff;
		lightingEffectParams.f[1] = lightingMaterial->fRimLightPower;
	}

	if (rawTechnique & RAW_FLAG_BACK_LIGHTING)
	{
		renderer->SetTexture(9, lightingMaterial->spSpecularBackLightingTexture);
		renderer->SetTextureAddressMode(9, lightingMaterial->eTextureClampMode);
	}

	if (rawTechnique & RAW_FLAG_SNOW)
	{
		// PS: p34 float4 SnowRimLightParameters
		XMVECTORF32& snowRimLightParameters = pixelCG.ParamPS<XMVECTORF32, 34>();

		snowRimLightParameters.f[0] = fSnowRimLightIntensity->uValue.f;
		snowRimLightParameters.f[1] = fSnowGeometrySpecPower->uValue.f;
		snowRimLightParameters.f[2] = fSnowNormalSpecPower->uValue.f;
		snowRimLightParameters.f[3] = bEnableSnowRimLighting->uValue.b ? 1.0f : 0.0f;
	}

	if (setDiffuseNormalSamplers)
	{
		if (lightingMaterial->iDiffuseRenderTargetSourceIndex == RENDER_TARGET_NONE)
		{
			MatSetTextureSlot(0, lightingMaterial->spDiffuseTexture, lightingMaterial);
		}
		else
		{
			renderer->SetShaderResource(0, renderer->Data.pRenderTargets[lightingMaterial->iDiffuseRenderTargetSourceIndex].SRV);
			renderer->SetTextureAddressMode(0, lightingMaterial->eTextureClampMode);
		}

		renderer->SetTexture(1, lightingMaterial->spNormalTexture);
		renderer->SetTextureAddressMode(1, lightingMaterial->eTextureClampMode);
	}

	// PS: p29 float4 IBLParams
	{
		XMVECTORF32& iblParams = pixelCG.ParamPS<XMVECTORF32, 29>();

		XMVECTORF32 thing;

		if (*(bool *)(v5 + 240))
			thing = *(XMVECTORF32 *)(v5 + 208);
		else
			thing = *(XMVECTORF32 *)(v5 + 224);

		iblParams.f[0] = *(float *)(v5 + 204);
		iblParams.f[1] = thing.f[0];
		iblParams.f[2] = thing.f[1];
		iblParams.f[3] = thing.f[2];
	}

	if (rawTechnique & RAW_FLAG_CHARACTER_LIGHT)
	{
		if (dword_141E33BA0 >= 0)
		{
			renderer->SetShaderResource(11, renderer->Data.pRenderTargets[sub_141314170(g_ModuleBase + 0x1E33B98)].SRV);
			renderer->SetTextureAddressMode(11, 0);
		}

		// PS: p35 float4 CharacterLightParams
		XMVECTOR& characterLightParams = pixelCG.ParamPS<XMVECTOR, 35>();

		if (BSShaderManager::St.CharacterLightEnabled)
			BSGraphics::Utility::CopyNiColorAToFloat(&characterLightParams, BSShaderManager::St.CharacterLightParams);
	}

	renderer->FlushConstantGroupVSPS(&vertexCG, &pixelCG);
	renderer->ApplyConstantGroupVSPS(&vertexCG, &pixelCG, BSGraphics::CONSTANT_GROUP_LEVEL_MATERIAL);
}

void BSLightingShader::RestoreMaterial(BSShaderMaterial const *Material)
{
	BSSHADER_FORWARD_CALL(MATERIAL, &BSLightingShader::RestoreMaterial, Material);
}

// BSUtilities::GetInverseWorldMatrix(const NiTransform& Transform, bool UseInputTransform, D3DMATRIX& Matrix)
void GetInverseWorldMatrix(const NiTransform& Transform, bool Skinned, XMMATRIX& OutMatrix)
{
	if (Skinned)
	{
		const NiPoint3 posAdjust = BSGraphics::Renderer::QInstance()->GetRendererShadowState()->m_PosAdjust;

		// XMMatrixIdentity(), row[3] = { world.x, world.y, world.z, 1.0f }, XMMatrixInverse()
		OutMatrix = XMMatrixInverse(nullptr, XMMatrixTranslation(posAdjust.x, posAdjust.y, posAdjust.z));
	}
	else
	{
		NiTransform inverted;
		Transform.Invert(inverted);

		OutMatrix = BSShaderUtil::GetXMFromNiPosAdjust(inverted, NiPoint3::ZERO);
	}
}

void BSLightingShader::SetupGeometry(BSRenderPass *Pass, uint32_t RenderFlags)
{
	BSSHADER_FORWARD_CALL(GEOMETRY, &BSLightingShader::SetupGeometry, Pass, RenderFlags);

	auto renderer = BSGraphics::Renderer::QInstance();
	auto state = renderer->GetRendererShadowState();

	auto property = static_cast<BSLightingShaderProperty *>(Pass->m_ShaderProperty);
	auto vertexCG = renderer->GetShaderConstantGroup(state->m_CurrentVertexShader, BSGraphics::CONSTANT_GROUP_LEVEL_GEOMETRY);
	auto pixelCG = renderer->GetShaderConstantGroup(state->m_CurrentPixelShader, BSGraphics::CONSTANT_GROUP_LEVEL_GEOMETRY);

	const uint32_t rawTechnique = TLS_m_CurrentRawTechnique;
	const uint32_t baseTechniqueID = (rawTechnique >> 24) & 0x3F;

	bool isSkinned = (rawTechnique & RAW_FLAG_SKINNED) != 0;
	bool isLOD = false;
	bool updateEyePosition = false;
	auto renderSpace = isSkinned ? Space::World : Space::Model;

	uint8_t v12 = Pass->m_AccumulationHint;
	uint8_t v103 = (unsigned __int8)(v12 - 2) <= 1u;

	if (v12 == 3 && property->GetFlag(BSShaderProperty::BSSP_FLAG_ZBUFFER_WRITE))
	{
		TLS_dword_141E3527C = renderer->AlphaBlendStateGetWriteMode();
		renderer->AlphaBlendStateSetWriteMode(1);
	}

	switch (baseTechniqueID)
	{
	case RAW_TECHNIQUE_ENVMAP:
	case RAW_TECHNIQUE_MULTILAYERPARALLAX:
	case RAW_TECHNIQUE_EYE:
	{
		// NOTE: The game updates view projection twice...? See the if() after this switch.
		if (!isSkinned)
			GeometrySetupConstantWorld(vertexCG, Pass->m_Geometry->GetWorldTransform(), false, nullptr);

		renderSpace = Space::World;
		updateEyePosition = true;

		// PS: p7 float4 MaterialData (NOTE: This is written AGAIN)
		auto& var = pixelCG.ParamPS<XMVECTORF32, 7>();
		var.f[0] = property->fEnvmapLODFade;
	}
	break;

	case RAW_TECHNIQUE_MTLAND:
	case RAW_TECHNIQUE_MTLANDLODBLEND:
	{
		auto material = static_cast<const BSLightingShaderMaterialLandscape *>(property->pMaterial);

		GeometrySetupConstantLandBlendParams(
			vertexCG,
			Pass->m_Geometry->GetWorldTranslate(),
			material->kLandBlendParams.r,
			material->kLandBlendParams.g);
	}
	break;

	case RAW_TECHNIQUE_LODLAND:
	case RAW_TECHNIQUE_LODOBJ:
	case RAW_TECHNIQUE_LODOBJHD:
	case RAW_TECHNIQUE_LODLANDNOISE:
		GeometrySetupConstantWorld(vertexCG, Pass->m_Geometry->GetWorldTransform(), false, nullptr);
		GeometrySetupConstantWorld(vertexCG, Pass->m_Geometry->GetWorldTransform(), true, &state->m_PreviousPosAdjust);
		isLOD = true;
		break;

	case RAW_TECHNIQUE_TREE:
		GeometrySetupConstantTreeParams(vertexCG, property);
		break;
	}

	if (!isSkinned && !isLOD)
	{
		// Unknown renderer flag: Determines if previous world projection is used
		const NiTransform& world = Pass->m_Geometry->GetWorldTransform();
		const NiTransform& temp = (RenderFlags & 0x10) ? world : Pass->m_Geometry->GetPreviousWorldTransform();

		GeometrySetupConstantWorld(vertexCG, world, false, nullptr);
		GeometrySetupConstantWorld(vertexCG, temp, true, &state->m_PreviousPosAdjust);
	}

	XMMATRIX inverseWorldMatrix;
	GetInverseWorldMatrix(Pass->m_Geometry->GetWorldTransform(), false, inverseWorldMatrix);

	GeometrySetupConstantDirectionalLight(pixelCG, Pass, inverseWorldMatrix, renderSpace);
	GeometrySetupConstantDirectionalAmbientLight(pixelCG, Pass->m_Geometry->GetWorldTransform().m_Rotate, renderSpace);

	// PS: p7 float4 MaterialData (Write #1)
	{
		XMVECTORF32& materialData = pixelCG.ParamPS<XMVECTORF32, 7>();

		if (Pass->m_LODMode.SingleLevel)
			materialData.f[2] = property->GetAlpha() * *(float *)((uintptr_t)property->pFadeNode + 332i64);
		else
			materialData.f[2] = property->GetAlpha();
	}

	GeometrySetupEmitColorConstants(pixelCG, property);

	uint32_t lightCount = (rawTechnique >> 3) & 0b111;		// 0 - 7
	uint32_t shadowLightCount = (rawTechnique >> 6) & 0b111;// 0 - 7

	// PS: p0 float2 NumLightNumShadowLight
	{
		XMFLOAT2& numLightNumShadowLight = pixelCG.ParamPS<XMFLOAT2, 0>();

		numLightNumShadowLight.x = (float)lightCount;
		numLightNumShadowLight.y = (float)shadowLightCount;
	}

	//
	// PS: p1 float4 PointLightPosition[7]
	// PS: p2 float4 PointLightColor[7]
	//
	// Original code just memset()'s these but it's already zeroed now. The values get
	// set up in the relevant function (GeometrySetupConstantPointLights).
	//
	//{
	//	auto& pointLightPosition = pixelCG.Param<XMVECTOR[7], 1>(ps);
	//	auto& pointLightColor = pixelCG.Param<XMVECTOR[7], 2>(ps);
	//
	//	memset(&pointLightPosition, 0, sizeof(XMVECTOR) * 7);
	//	memset(&pointLightColor, 0, sizeof(XMVECTOR) * 7);
	//}
	//

	if (lightCount > 0)
	{
		float specularScale = Pass->m_Geometry->GetWorldTransform().m_fScale;

		if (baseTechniqueID == RAW_TECHNIQUE_ENVMAP || baseTechniqueID == RAW_TECHNIQUE_EYE)
			specularScale = 1.0f;

		GeometrySetupConstantPointLights(pixelCG, Pass, inverseWorldMatrix, lightCount, shadowLightCount, specularScale, renderSpace);
	}

	if (rawTechnique & RAW_FLAG_SPECULAR)
	{
		// PS: p7 float4 MaterialData (Write #2)
		XMVECTORF32& materialData = pixelCG.ParamPS<XMVECTORF32, 7>();

		materialData.f[1] = property->fSpecularLODFade;
		updateEyePosition = true;
	}

	if (rawTechnique & (RAW_FLAG_SOFT_LIGHTING | RAW_FLAG_RIM_LIGHTING | RAW_FLAG_BACK_LIGHTING | RAW_FLAG_AMBIENT_SPECULAR))
		updateEyePosition = true;

	if ((rawTechnique & RAW_FLAG_PROJECTED_UV) && (baseTechniqueID != RAW_TECHNIQUE_HAIR))
	{
		bool enableProjectedUvNormals = bEnableProjecteUVDiffuseNormals->uValue.b && (!(RenderFlags & 0x8) || !bEnableProjecteUVDiffuseNormalsOnCubemap->uValue.b);
		XMMATRIX textureProjectionTemp;

		renderer->SetTexture(11, BSGraphics::gState.pDefaultTextureProjNoiseMap);
		renderer->SetTextureMode(11, 3, 1);

		if (enableProjectedUvNormals && BSGraphics::gState.pDefaultTextureProjDiffuseMap)
		{
			renderer->SetTexture(3, BSGraphics::gState.pDefaultTextureProjDiffuseMap);
			renderer->SetTextureMode(3, 3, 1);

			renderer->SetTexture(8, BSGraphics::gState.pDefaultTextureProjNormalMap);
			renderer->SetTextureMode(8, 3, 1);

			renderer->SetTexture(10, BSGraphics::gState.pDefaultTextureProjNormalDetailMap);
			renderer->SetTextureMode(10, 3, 1);
		}

		if (auto multiIndexShape = Pass->m_Geometry->IsMultiIndexTriShape())
		{
			textureProjectionTemp = XMLoadFloat4x4((XMFLOAT4X4 *)&multiIndexShape->MaterialProjection);
			GeometrySetupConstantProjectedUVData(pixelCG, multiIndexShape, property, enableProjectedUvNormals);
		}
		else
		{
			GenerateProjectionMatrix(Pass->m_Geometry->GetWorldTransform(), textureProjectionTemp, baseTechniqueID == RAW_TECHNIQUE_ENVMAP);
			GeometrySetupConstantProjectedUVData(pixelCG, nullptr, property, enableProjectedUvNormals);
		}

		// VS: p6 float3x4 textureProj
		BSShaderUtil::TransposeStoreMatrix3x4(&vertexCG.ParamVS<float, 6>(), textureProjectionTemp);
	}

	if (rawTechnique & RAW_FLAG_WORLD_MAP)
	{
		renderer->SetTexture(12, WorldMapOverlayNormalTexture);
		renderer->SetTextureAddressMode(12, 3);

		renderer->SetTexture(13, WorldMapOverlayNormalSnowTexture);
		renderer->SetTextureAddressMode(13, 3);

		// VS: p8 float4 WorldMapOverlayParameters
		BSGraphics::Utility::CopyNiColorAToFloat(&vertexCG.ParamVS<XMVECTOR, 8>(), dword_1431F5540);

		// PS: p17 float4 WorldMapOverlayParametersPS
		BSGraphics::Utility::CopyNiColorAToFloat(&pixelCG.ParamPS<XMVECTOR, 17>(), dword_1431F5550);
	}

	// VS: p2 float3 EyePosition
	if (updateEyePosition)
	{
		XMFLOAT3& eyePosition = vertexCG.ParamVS<XMFLOAT3, 2>();

		if (renderSpace == Space::Model)
		{
			XMVECTOR coord = BSShaderManager::GetCurrentAccumulator()->m_EyePosition.AsXmm();
			XMStoreFloat3(&eyePosition, XMVector3TransformCoord(coord, inverseWorldMatrix));
		}
		else
		{
			auto& position = BSShaderManager::GetCurrentAccumulator()->m_EyePosition;

			eyePosition.x = position.x - state->m_PosAdjust.x;
			eyePosition.y = position.y - state->m_PosAdjust.y;
			eyePosition.z = position.z - state->m_PosAdjust.z;
		}
	}

	if (Pass->m_AccumulationHint == 10)
	{
		uintptr_t v89 = (uintptr_t)property->pFadeNode;
		float v90;
		
		if (Pass->m_LODMode.SingleLevel)
			v90 = *(float *)(v89 + 332) * 31.0f;
		else
			v90 = *(float *)(v89 + 304) * 31.0f;

		renderer->DepthStencilStateSetStencilMode(11, (uint32_t)v90 & 0xFF);
	}

	if (!v103)
	{
		auto oldDepthMode = renderer->DepthStencilStateGetDepthMode();

		if (!property->GetFlag(BSShaderProperty::BSSP_FLAG_ZBUFFER_WRITE))
		{
			TLS_dword_141E35280 = oldDepthMode;
			renderer->DepthStencilStateSetDepthMode(DEPTH_STENCIL_DEPTH_MODE_TEST);
		}

		if (!property->GetFlag(BSShaderProperty::BSSP_FLAG_ZBUFFER_TEST))
		{
			TLS_dword_141E35280 = oldDepthMode;
			renderer->DepthStencilStateSetDepthMode(DEPTH_STENCIL_DEPTH_MODE_DISABLED);
		}
	}

	// PS: p16 float4 SSRParams
	{
		XMVECTORF32& ssrParams = pixelCG.ParamPS<XMVECTORF32, 16>();

		ssrParams.f[0] = fSpecMaskBegin->uValue.f;
		ssrParams.f[1] = fSpecMaskSpan->uValue.f + fSpecMaskBegin->uValue.f;
		ssrParams.f[2] = flt_143257C40;

		float v98 = 0.0f;
		float v99 = 0.0f;

		if (rawTechnique & RAW_FLAG_SPECULAR)
			v99 = property->fSpecularLODFade;

		if ((RenderFlags & 0x2) == 0)
			v98 = 1.0f;

		ssrParams.f[3] = v98 * v99;
	}

	renderer->FlushConstantGroupVSPS(&vertexCG, &pixelCG);
	renderer->ApplyConstantGroupVSPS(&vertexCG, &pixelCG, BSGraphics::CONSTANT_GROUP_LEVEL_GEOMETRY);
}

void BSLightingShader::RestoreGeometry(BSRenderPass *Pass, uint32_t RenderFlags)
{
	BSSHADER_FORWARD_CALL(GEOMETRY, &BSLightingShader::RestoreGeometry, Pass, RenderFlags);

	auto renderer = BSGraphics::Renderer::QInstance();

	if (Pass->m_AccumulationHint == 10)
		renderer->DepthStencilStateSetStencilMode(DEPTH_STENCIL_STENCIL_MODE_DEFAULT, 255);

	if (TLS_dword_141E35280 != DEPTH_STENCIL_DEPTH_MODE_TESTGREATER)
	{
		renderer->DepthStencilStateSetDepthMode(TLS_dword_141E35280);
		TLS_dword_141E35280 = DEPTH_STENCIL_DEPTH_MODE_TESTGREATER;
	}

	if (TLS_dword_141E3527C != 13)
	{
		renderer->AlphaBlendStateSetWriteMode(TLS_dword_141E3527C);
		TLS_dword_141E3527C = 13;
	}
}

void BSLightingShader::CreateAllShaders()
{
	for (auto itr = m_VertexShaderTable.begin(); itr != m_VertexShaderTable.end(); itr++)
	{
		// Apply to parallax shaders only
		//if (((itr->m_TechniqueID >> 24) & 0x3F) != RAW_TECHNIQUE_PARALLAX)
		//	continue;

		switch ((itr->m_TechniqueID >> 24) & 0x3F)
		{
		case RAW_TECHNIQUE_MTLAND:
		case RAW_TECHNIQUE_MTLANDLODBLEND:
		case RAW_TECHNIQUE_LODLAND:
		case RAW_TECHNIQUE_LODLANDNOISE:
			break;

		default:
			continue;
		}

		auto defines = GetSourceDefines(itr->m_TechniqueID);
		auto getConstant = [](int i) { return ShaderConfigLighting.ByConstantIndexVS.count(i) ? ShaderConfigLighting.ByConstantIndexVS.at(i)->Name : nullptr; };

		CreateVertexShader(itr->m_TechniqueID, "Lighting", defines, getConstant);
		CreateHullShader(itr->m_TechniqueID, "Lighting", defines);
		CreateDomainShader(itr->m_TechniqueID, "Lighting", defines);
	}
}

uint32_t BSLightingShader::GetRawTechnique(uint32_t Technique)
{
	uint32_t outputTech = Technique - 0x4800002D;
	uint32_t subIndex = (outputTech >> 24) & 0x3F;

	if (subIndex == RAW_TECHNIQUE_LODLANDNOISE && !BSShaderManager::bLODLandscapeNoise)
	{
		outputTech = outputTech & 0xC9FFFFFF | 0x9000000;
	}
	else if (subIndex == RAW_TECHNIQUE_PARALLAXOCC && !bEnableParallaxOcclusion->uValue.b)
	{
		outputTech &= 0xC0FFFFFF;
	}

	return outputTech;
}

uint32_t BSLightingShader::GetVertexTechnique(uint32_t RawTechnique)
{
	uint32_t flags = RawTechnique & (
		RAW_FLAG_VC |
		RAW_FLAG_SKINNED |
		RAW_FLAG_MODELSPACENORMALS |
		RAW_FLAG_PROJECTED_UV |
		RAW_FLAG_WORLD_MAP);

	if (RawTechnique & (RAW_FLAG_SPECULAR | RAW_FLAG_RIM_LIGHTING | RAW_FLAG_AMBIENT_SPECULAR))
		flags |= RAW_FLAG_SPECULAR;

	return (RawTechnique & 0x3F000000) | flags;
}

uint32_t BSLightingShader::GetPixelTechnique(uint32_t RawTechnique)
{
	uint32_t flags = RawTechnique & ~(
		RAW_FLAG_LIGHTCOUNT1 |
		RAW_FLAG_LIGHTCOUNT2 |
		RAW_FLAG_LIGHTCOUNT3 |
		RAW_FLAG_LIGHTCOUNT4 |
		RAW_FLAG_LIGHTCOUNT5 |
		RAW_FLAG_LIGHTCOUNT6);

	if ((flags & RAW_FLAG_MODELSPACENORMALS) == 0)
		flags &= ~RAW_FLAG_SKINNED;

	return flags | RAW_FLAG_VC;
}

std::vector<std::pair<const char*, const char*>> BSLightingShader::GetSourceDefines(uint32_t Technique)
{
	std::vector<std::pair<const char *, const char *>> defines;
	uint32_t subType = (Technique >> 24) & 0x3F;

	if (Technique & RAW_FLAG_VC) defines.emplace_back("VC", "");
	if (Technique & RAW_FLAG_SKINNED) defines.emplace_back("SKINNED", "");
	if (Technique & RAW_FLAG_MODELSPACENORMALS) defines.emplace_back("MODELSPACENORMALS", "");
	if (Technique & RAW_FLAG_SPECULAR) defines.emplace_back("SPECULAR", "");
	if (Technique & RAW_FLAG_SOFT_LIGHTING) defines.emplace_back("SOFT_LIGHTING", "");
	if (Technique & RAW_FLAG_SHADOW_DIR) defines.emplace_back("SHADOW_DIR", "");
	if (Technique & RAW_FLAG_DEFSHADOW) defines.emplace_back("DEFSHADOW", "");
	if (Technique & RAW_FLAG_RIM_LIGHTING) defines.emplace_back("RIM_LIGHTING", "");
	if (Technique & RAW_FLAG_BACK_LIGHTING) defines.emplace_back("BACK_LIGHTING", "");
	if ((Technique & RAW_FLAG_PROJECTED_UV) && subType != RAW_TECHNIQUE_HAIR) defines.emplace_back("PROJECTED_UV", "");
	if (Technique & RAW_FLAG_ANISO_LIGHTING) defines.emplace_back("ANISO_LIGHTING", "");
	if (Technique & RAW_FLAG_AMBIENT_SPECULAR) defines.emplace_back("AMBIENT_SPECULAR", "");
	if (Technique & RAW_FLAG_WORLD_MAP) defines.emplace_back("WORLD_MAP", "");
	if (Technique & RAW_FLAG_DO_ALPHA_TEST) defines.emplace_back("DO_ALPHA_TEST", "");
	if (Technique & RAW_FLAG_SNOW) defines.emplace_back("SNOW", "");
	if (Technique & RAW_FLAG_BASE_OBJECT_IS_SNOW) defines.emplace_back("BASE_OBJECT_IS_SNOW", "");
	if (Technique & RAW_FLAG_CHARACTER_LIGHT) defines.emplace_back("CHARACTER_LIGHT", "");
	if ((Technique & RAW_FLAG_PROJECTED_UV) && subType == RAW_TECHNIQUE_HAIR) defines.emplace_back("DEPTH_WRITE_DECALS", "");
	if (Technique & RAW_FLAG_ADDITIONAL_ALPHA_MASK) defines.emplace_back("ADDITIONAL_ALPHA_MASK", "");

	switch (subType)
	{
	case RAW_TECHNIQUE_NONE: /* Nothing */ break;
	case RAW_TECHNIQUE_ENVMAP: defines.emplace_back("ENVMAP", ""); break;
	case RAW_TECHNIQUE_GLOWMAP: defines.emplace_back("GLOWMAP", ""); break;
	case RAW_TECHNIQUE_PARALLAX: defines.emplace_back("PARALLAX", ""); break;
	case RAW_TECHNIQUE_FACEGEN: defines.emplace_back("FACEGEN", ""); break;
	case RAW_TECHNIQUE_FACEGENRGBTINT: defines.emplace_back("FACEGEN_RGB_TINT", ""); break;
	case RAW_TECHNIQUE_HAIR: defines.emplace_back("HAIR", ""); break;
	case RAW_TECHNIQUE_PARALLAXOCC: defines.emplace_back("PARALLAX_OCC", ""); break;
	case RAW_TECHNIQUE_MTLAND: defines.emplace_back("MULTI_TEXTURE", ""); defines.emplace_back("LANDSCAPE", ""); break;
	case RAW_TECHNIQUE_LODLAND: defines.emplace_back("LODLANDSCAPE", ""); break;
	case RAW_TECHNIQUE_SNOW: /* Nothing */ break;
	case RAW_TECHNIQUE_MULTILAYERPARALLAX: defines.emplace_back("MULTI_LAYER_PARALLAX", ""); defines.emplace_back("ENVMAP", ""); break;
	case RAW_TECHNIQUE_TREE: defines.emplace_back("TREE_ANIM", ""); break;
	case RAW_TECHNIQUE_LODOBJ: defines.emplace_back("LODOBJECTS", ""); break;
	case RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW: defines.emplace_back("MULTI_INDEX", ""); defines.emplace_back("SPARKLE", ""); break;
	case RAW_TECHNIQUE_LODOBJHD: defines.emplace_back("LODOBJECTSHD", ""); break;
	case RAW_TECHNIQUE_EYE: defines.emplace_back("EYE", ""); break;
	case RAW_TECHNIQUE_CLOUD: defines.emplace_back("CLOUD", ""); defines.emplace_back("INSTANCED", ""); break;
	case RAW_TECHNIQUE_LODLANDNOISE: defines.emplace_back("LODLANDSCAPE", ""); defines.emplace_back("LODLANDNOISE", ""); break;
	case RAW_TECHNIQUE_MTLANDLODBLEND: defines.emplace_back("MULTI_TEXTURE", ""); defines.emplace_back("LANDSCAPE", ""); defines.emplace_back("LOD_LAND_BLEND", ""); break;
	default: Assert(false); break;
	}

	return defines;
}

std::string BSLightingShader::GetTechniqueString(uint32_t Technique)
{
	char buffer[1024];
	sprintf_s(buffer, "%dSh%d ", (Technique >> 3) & 7, (Technique >> 6) & 7);
	uint32_t subType = (Technique >> 24) & 0x3F;

	if (Technique & RAW_FLAG_VC)
		strcat_s(buffer, "Vc ");
	if (Technique & RAW_FLAG_SKINNED)
		strcat_s(buffer, "Sk ");
	if (Technique & RAW_FLAG_MODELSPACENORMALS)
		strcat_s(buffer, "Msn ");
	if (Technique & RAW_FLAG_SPECULAR)
		strcat_s(buffer, "Spc ");
	if (Technique & RAW_FLAG_SOFT_LIGHTING)
		strcat_s(buffer, "Sss ");
	if (Technique & RAW_FLAG_SHADOW_DIR)
		strcat_s(buffer, "Shd ");
	if (Technique & RAW_FLAG_DEFSHADOW)
		strcat_s(buffer, "DfSh ");
	if (Technique & RAW_FLAG_RIM_LIGHTING)
		strcat_s(buffer, "Rim ");
	if (Technique & RAW_FLAG_BACK_LIGHTING)
		strcat_s(buffer, "Bk ");
	if ((Technique & RAW_FLAG_PROJECTED_UV) && subType != RAW_TECHNIQUE_HAIR)
		strcat_s(buffer, "Projuv ");
	if (Technique & RAW_FLAG_AMBIENT_SPECULAR)
		strcat_s(buffer, "Aspc ");
	if (Technique & RAW_FLAG_WORLD_MAP)
		strcat_s(buffer, "Wmap ");
	if (Technique & RAW_FLAG_DO_ALPHA_TEST)
		strcat_s(buffer, "Atest ");
	if (Technique & RAW_FLAG_SNOW)
		strcat_s(buffer, "Snow ");
	if (Technique & RAW_FLAG_BASE_OBJECT_IS_SNOW)
		strcat_s(buffer, "BaseSnow ");
	if ((Technique & RAW_FLAG_PROJECTED_UV) && subType == RAW_TECHNIQUE_HAIR)
		strcat_s(buffer, "DwDecals ");
	if (Technique & RAW_FLAG_ADDITIONAL_ALPHA_MASK)
		strcat_s(buffer, "Aam ");

	switch (subType)
	{
	case RAW_TECHNIQUE_NONE: strcat_s(buffer, "None"); break;
	case RAW_TECHNIQUE_ENVMAP: strcat_s(buffer, "Envmap"); break;
	case RAW_TECHNIQUE_GLOWMAP: strcat_s(buffer, "Glowmap"); break;
	case RAW_TECHNIQUE_PARALLAX: strcat_s(buffer, "Parallax"); break;
	case RAW_TECHNIQUE_FACEGEN: strcat_s(buffer, "Facegen"); break;
	case RAW_TECHNIQUE_FACEGENRGBTINT: strcat_s(buffer, "FacegenRGBTint"); break;
	case RAW_TECHNIQUE_HAIR: strcat_s(buffer, "Hair"); break;
	case RAW_TECHNIQUE_PARALLAXOCC: strcat_s(buffer, "ParallaxOcc"); break;
	case RAW_TECHNIQUE_MTLAND: strcat_s(buffer, "MTLand"); break;
	case RAW_TECHNIQUE_LODLAND: strcat_s(buffer, "LODLand"); break;
	case RAW_TECHNIQUE_SNOW: /* No subtype defined in their code */ break;
	case RAW_TECHNIQUE_MULTILAYERPARALLAX: strcat_s(buffer, "MultiLayerParallax"); break;
	case RAW_TECHNIQUE_TREE: strcat_s(buffer, "Tree"); break;
	case RAW_TECHNIQUE_LODOBJ: strcat_s(buffer, "LODObj"); break;
	case RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW: strcat_s(buffer, "MultiIndexTriShapeSnow"); break;
	case RAW_TECHNIQUE_LODOBJHD: strcat_s(buffer, "LODObjHD"); break;
	case RAW_TECHNIQUE_EYE: strcat_s(buffer, "Eye"); break;
	case RAW_TECHNIQUE_CLOUD: /* No subtype defined in their code */ break;
	case RAW_TECHNIQUE_LODLANDNOISE: strcat_s(buffer, "LODLandNoise"); break;
	case RAW_TECHNIQUE_MTLANDLODBLEND: strcat_s(buffer, "MTLandLODBlend"); break;
	default: Assert(false); break;
	}

	XUtil::Trim(buffer, ' ');
	return buffer;
}

void BSLightingShader::TechUpdateHighDetailRangeConstants(BSGraphics::VertexCGroup& VertexCG)
{
	auto state = BSGraphics::Renderer::QInstance()->GetRendererShadowState();

	// VS: p12 float4 HighDetailRange
	BSGraphics::Utility::CopyNiColorAToFloat(&VertexCG.ParamVS<XMVECTOR, 12>(),
		NiColorA(
			BSShaderManager::St.LoadedRange.r - state->m_PosAdjust.x,
			BSShaderManager::St.LoadedRange.g - state->m_PosAdjust.y,
			BSShaderManager::St.LoadedRange.b - 15.0f,
			BSShaderManager::St.LoadedRange.a - 15.0f));
}

void BSLightingShader::TechUpdateFogConstants(BSGraphics::VertexCGroup& VertexCG, BSGraphics::PixelCGroup& PixelCG)
{
	auto fogParams = BSShaderManager::GetCurrentFogProperty();

	if (!fogParams)
		return;

	// Set both vertex & pixel here
	{
		XMVECTORF32 fogColor;
		fogColor.f[0] = fogParams->m_kNearColor.r;
		fogColor.f[1] = fogParams->m_kNearColor.g;
		fogColor.f[2] = fogParams->m_kNearColor.b;
		fogColor.f[3] = BSShaderManager::St.fInvFrameBufferRange;

		// VS: p14 float4 FogNearColor
		VertexCG.ParamVS<XMVECTORF32, 14>() = fogColor;

		// PS: p19 float4 FogColor
		PixelCG.ParamPS<XMVECTORF32, 19>() = fogColor;
	}

	// VS: p15 float4 FogFarColor
	XMVECTORF32& fogFarColor = VertexCG.ParamVS<XMVECTORF32, 15>();
	fogFarColor.f[0] = fogParams->m_kFarColor.r;
	fogFarColor.f[1] = fogParams->m_kFarColor.g;
	fogFarColor.f[2] = fogParams->m_kFarColor.b;
	fogFarColor.f[3] = 0.0f;

	// VS: p13 float4 FogParam
	XMVECTORF32& fogParam = VertexCG.ParamVS<XMVECTORF32, 13>();

	if (fogParams->fEndDistance != 0.0f || fogParams->fStartDistance != 0.0f)
	{
		float invRange = 1.0f / (fogParams->fEndDistance - fogParams->fStartDistance);

		fogParam.f[0] = invRange * fogParams->fStartDistance;
		fogParam.f[1] = invRange;
		fogParam.f[2] = fogParams->fPower;
		fogParam.f[3] = fogParams->fClamp;
	}
	else
	{
		fogParam = xmmword_14187D940;
	}
}

void BSLightingShader::MatSetEnvTexture(const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material)
{
	auto renderer = BSGraphics::Renderer::QInstance();

	renderer->SetTexture(4, Texture);
	renderer->SetTextureAddressMode(4, Material->eTextureClampMode);
}

void BSLightingShader::MatSetEnvMaskTexture(const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material)
{
	auto renderer = BSGraphics::Renderer::QInstance();

	renderer->SetTexture(5, Texture ? Texture : BSGraphics::gState.pDefaultTextureNormalMap);
	renderer->SetTextureAddressMode(5, Material->eTextureClampMode);
}

void BSLightingShader::MatSetTextureSlot(int Slot, const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material)
{
	auto renderer = BSGraphics::Renderer::QInstance();

	renderer->SetTexture(Slot, Texture);
	renderer->SetTextureAddressMode(Slot, Material->eTextureClampMode);
}

void BSLightingShader::MatSetMultiTextureLandOverrides(const BSLightingShaderMaterialLandscape *Material)
{
	auto renderer = BSGraphics::Renderer::QInstance();

	// This overrides all 12 samplers for land parameters if set in the property (max 6 diffuse, 6 normal)
	renderer->SetTexture(0, Material->spDiffuseTexture);
	renderer->SetTexture(7, Material->spNormalTexture);

	Assert(Material->uiNumLandscapeTextures <= ARRAYSIZE(BSLightingShaderMaterialLandscape::spLandscapeDiffuseTexture));
	Assert(Material->uiNumLandscapeTextures <= ARRAYSIZE(BSLightingShaderMaterialLandscape::spLandscapeNormalTexture));

	for (uint32_t i = 0; i < Material->uiNumLandscapeTextures; i++)
	{
		renderer->SetTexture(i + 1, Material->spLandscapeDiffuseTexture[i]);
		renderer->SetTextureAddressMode(i + 1, 3);

		renderer->SetTexture(i + 8, Material->spLandscapeNormalTexture[i]);
		renderer->SetTextureAddressMode(i + 8, 3);
	}

	if (Material->spTerrainOverlayTexture)
	{
		renderer->SetTexture(13, Material->spTerrainOverlayTexture);
		renderer->SetTextureAddressMode(13, 0);
	}
}

__int64 BSLightingShader::sub_141314170(__int64 a1)
{
	return *(unsigned int *)(a1 + 8);
}

void BSLightingShader::GeometrySetupConstantWorld(BSGraphics::VertexCGroup& VertexCG, const NiTransform& Transform, bool IsPreviousWorld, const NiPoint3 *PosAdjust)
{
	//
	// Instead of using the typical 4x4 matrix like everywhere else, someone decided that the
	// lighting shader is going to use 3x4 matrices. The missing 4th column is assumed to be
	// { 0, 0, 0, 1 } in row-major form.
	//
	XMMATRIX projMatrix;

	if (PosAdjust)
		projMatrix = BSShaderUtil::GetXMFromNiPosAdjust(Transform, *PosAdjust);
	else
		projMatrix = BSShaderUtil::GetXMFromNi(Transform);

	//
	// VS: p0 float3x4 World
	// -- or --
	// VS: p1 float3x4 PreviousWorld
	//
	if (!IsPreviousWorld)
		BSShaderUtil::TransposeStoreMatrix3x4(&VertexCG.ParamVS<float, 0>(), projMatrix);
	else
		BSShaderUtil::TransposeStoreMatrix3x4(&VertexCG.ParamVS<float, 1>(), projMatrix);
}

void BSLightingShader::GeometrySetupConstantLandBlendParams(const BSGraphics::VertexCGroup& VertexCG, const NiPoint3& Translate, float OffsetX, float OffsetY)
{
	float alpha = 0.0f;
	float fadeTime = (BSShaderManager::St.fTimerValues[BSShaderManager::TIMER_MODE_DEFAULT] - BSShaderManager::St.kfGriddArrayLerpStart) / (BSShaderManager::St.fLandLOFadeSeconds * 5.0f);

	if (fadeTime >= 0.0f)
		alpha = fmin(1.0f, fadeTime);

	float deltaX = BSShaderManager::St.kGridArrayCenter.x - BSShaderManager::St.kOldGridArrayCenter.x;
	float deltaY = BSShaderManager::St.kGridArrayCenter.y - BSShaderManager::St.kOldGridArrayCenter.y;

	// BSShaderManager::bLODFadeInProgress
	if (alpha == 1.0f)
		byte_1431F547C = false;

	// VS: p3 float4 LandBlendParams
	XMVECTORF32& landBlendParams = VertexCG.ParamVS<XMVECTORF32, 3>();

	landBlendParams.f[0] = OffsetX;
	landBlendParams.f[1] = OffsetY;
	landBlendParams.f[2] = (BSShaderManager::St.kOldGridArrayCenter.x + (deltaX * alpha)) - Translate.x;
	landBlendParams.f[3] = (BSShaderManager::St.kOldGridArrayCenter.y + (deltaY * alpha)) - Translate.y;
}

void BSLightingShader::GeometrySetupConstantTreeParams(const BSGraphics::VertexCGroup& VertexCG, BSLightingShaderProperty *Property)
{
	uintptr_t leafAnimNode = 0;

	if (Property->pFadeNode && Property->pFadeNode != qword_1431F5410)
		leafAnimNode = (*(__int64(__cdecl **)())(*(uintptr_t *)Property->pFadeNode + 0x1F8))();

	// VS: p4 float4 TreeParams
	XMVECTORF32& treeParams = VertexCG.ParamVS<XMVECTORF32, 4>();
	{
		float invDistToCamera = 0.0f;
		float amplitude = 1.0f;
		float leafFrequency = 1.0f;

		if (leafAnimNode)
		{
			invDistToCamera = 1.0f / sqrt(*(float *)(leafAnimNode + 0x158));
			amplitude = *(float *)(leafAnimNode + 0x15C);
			leafFrequency = *(float *)(leafAnimNode + 0x160);
		}

		float dampenStart = BSShaderManager::St.fLeafAnimDampenDistStartSPU;
		float dampenEnd = BSShaderManager::St.fLeafAnimDampenDistEndSPU;
		float amplitudeLimit = std::max((1.0f - ((invDistToCamera - dampenStart) / (dampenEnd - dampenStart))) * amplitude, 0.0f);

		treeParams.f[0] = 0.0f;// Timer?
		treeParams.f[1] = *(float *)((uintptr_t)BSShaderManager::St.pShadowSceneNode[0] + 0x304);// Wind magnitude
		treeParams.f[2] = std::min(amplitude, amplitudeLimit);
		treeParams.f[3] = leafFrequency;
	}

	// VS: p5 float2 WindTimers
	XMFLOAT2& windTimers = VertexCG.ParamVS<XMFLOAT2, 5>();
	{
		windTimers.x = 0.0f;
		windTimers.y = 0.0f;

		if (leafAnimNode)
		{
			windTimers.x = *(float *)(leafAnimNode + 0x164) * 6.0f;
			windTimers.y = *(float *)(leafAnimNode + 0x168) * 6.0f;
		}
	}
}

void BSLightingShader::GeometrySetupConstantDirectionalLight(const BSGraphics::PixelCGroup& PixelCG, const BSRenderPass *Pass, XMMATRIX& InvWorld, Space RenderSpace)
{
	BSLight *bsLight = Pass->QLights()[0];
	NiDirectionalLight *sunDirectionalLight = static_cast<NiDirectionalLight *>(bsLight->GetLight());

	float v12 = *(float *)(qword_1431F5810 + 224) * sunDirectionalLight->GetDimmer();

	XMFLOAT3& dirLightColor = PixelCG.ParamPS<XMFLOAT3, 4>();		// PS: p4 float3 DirLightColor
	XMFLOAT3& dirLightDirection = PixelCG.ParamPS<XMFLOAT3, 3>();	// PS: p3 float3 DirLightDirection

	dirLightColor.x = v12 * sunDirectionalLight->GetDiffuseColor().r;
	dirLightColor.y = v12 * sunDirectionalLight->GetDiffuseColor().g;
	dirLightColor.z = v12 * sunDirectionalLight->GetDiffuseColor().b;

	XMVECTOR lightDir = XMVectorNegate(sunDirectionalLight->GetWorldDirection().AsXmm());

	if (RenderSpace == Space::Model)
		lightDir = XMVector3TransformNormal(lightDir, InvWorld);

	XMStoreFloat3(&dirLightDirection, XMVector3Normalize(lightDir));
}

void BSLightingShader::GeometrySetupConstantDirectionalAmbientLight(const BSGraphics::PixelCGroup& PixelCG, const NiMatrix3& ModelToWorld, Space RenderSpace)
{
	// PS: p5 float3x4 DirectionalAmbient
	auto directionalAmbient = PixelCG.ParamPS<float[3][4], 5>();

	NiTransform xform = BSShaderManager::St.DirectionalAmbientTransform;

	if (RenderSpace == Space::Model)
		xform.m_Rotate = xform.m_Rotate * ModelToWorld;

	BSShaderUtil::StoreTransform3x4NoScale(directionalAmbient, xform);
}

void BSLightingShader::GeometrySetupEmitColorConstants(const BSGraphics::PixelCGroup& PixelCG, BSLightingShaderProperty *Property)
{
	// PS: p8 float3 EmitColor
	XMFLOAT3& emitColor = PixelCG.ParamPS<XMFLOAT3, 8>();

	emitColor.x = Property->pEmitColor->r * Property->fEmitColorScale;
	emitColor.y = Property->pEmitColor->g * Property->fEmitColorScale;
	emitColor.z = Property->pEmitColor->b * Property->fEmitColorScale;
}

void BSLightingShader::GeometrySetupConstantPointLights(const BSGraphics::PixelCGroup& PixelCG, BSRenderPass *Pass, XMMATRIX& Transform, uint32_t LightCount, uint32_t ShadowLightCount, float WorldScale, Space RenderSpace)
{
	AssertMsg(BSShaderManager::GetRenderMode() != 22 && BSShaderManager::GetRenderMode() != 17, "This code path was removed and should never be called!");
	AssertMsg(ShadowLightCount <= 4, "Shader only expects shadow selector data to fit in a FLOAT4");

	auto& pointLightPosition = PixelCG.ParamPS<XMVECTORF32[7], 1>();// PS: p1 float4[7] PointLightPosition
	auto& pointLightColor = PixelCG.ParamPS<XMVECTORF32[7], 2>();	// PS: p2 float4[7] PointLightColor
	auto& shadowLightMaskSelect = PixelCG.ParamPS<float[4], 10>();	// PS: p10 float4 ShadowLightMaskSelect

	for (uint32_t i = 0; i < LightCount; i++)
	{
		BSLight *screenSpaceLight = Pass->QLights()[i + 1];
		NiLight *niLight = screenSpaceLight->GetLight();

		AssertMsgDebug(niLight, "If the SSL is non-null, the NiLight should also be non-null.");

		NiPoint3 worldPos = niLight->GetWorldTranslate();
		float dimmer = niLight->GetDimmer() * screenSpaceLight->GetLODDimmer();

		if (BSShaderManager::St.bLiteBrite)
			dimmer = 0.0f;

		pointLightColor[i].f[0] = dimmer * niLight->GetDiffuseColor().r;
		pointLightColor[i].f[1] = dimmer * niLight->GetDiffuseColor().g;
		pointLightColor[i].f[2] = dimmer * niLight->GetDiffuseColor().b;

		if (RenderSpace == Space::Model)
		{
			pointLightPosition[i].v = DirectX::XMVector3TransformCoord(worldPos.AsXmm(), Transform);
			pointLightPosition[i].f[3] = niLight->GetSpecularColor().r / WorldScale;
		}
		else
		{
			worldPos = worldPos - BSGraphics::Renderer::QInstance()->GetRendererShadowState()->m_PosAdjust;

			pointLightPosition[i].v = worldPos.AsXmm();
			pointLightPosition[i].f[3] = niLight->GetSpecularColor().r;
		}

		if (i < ShadowLightCount)
			shadowLightMaskSelect[i] = (float)static_cast<BSShadowLight *>(screenSpaceLight)->UnkDword520;
	}
}

void BSLightingShader::GeometrySetupConstantProjectedUVData(const BSGraphics::PixelCGroup& PixelCG, BSMultiIndexTriShape *Shape, BSLightingShaderProperty *Property, bool EnableProjectedNormals)
{
	// PS: p12 float4 ProjectedUVParams
	{
		XMVECTORF32& projectedUVParams = PixelCG.ParamPS<XMVECTORF32, 12>();

		const NiColorA& params = Shape ? Shape->fMaterialParams : Property->QProjectedUVParams();

		projectedUVParams.f[0] = (1.0f - params.a) * params.r;
		//projectedUVParams.f[1] = ;
		projectedUVParams.f[2] = params.b;
		projectedUVParams.f[3] = ((1.0f - params.a) * params.g) + params.a;
	}

	// PS: p13 float4 ProjectedUVParams2
	{
		XMVECTORF32& projectedUVParams2 = PixelCG.ParamPS<XMVECTORF32, 13>();

		if (Shape)
		{
			projectedUVParams2.f[0] = Shape->fNormalDampener;
			projectedUVParams2.f[1] = Shape->fMaterialScale;
		}
		else
		{
			BSGraphics::Utility::CopyNiColorAToFloat(&projectedUVParams2.f[0], Property->QProjectedUVColor());
		}
	}

	// PS: p14 float4 ProjectedUVParams3
	{
		XMVECTORF32& projectedUVParams3 = PixelCG.ParamPS<XMVECTORF32, 14>();

		projectedUVParams3.f[0] = fProjectedUVDiffuseNormalTilingScale->uValue.f;
		projectedUVParams3.f[1] = fProjectedUVNormalDetailTilingScale->uValue.f;
		projectedUVParams3.f[2] = 0.0f;
		projectedUVParams3.f[3] = (EnableProjectedNormals) ? 1.0f : 0.0f;
	}
}

void BSLightingShader::GenerateProjectionMatrix(const NiTransform& ObjectWorldTrans, XMMATRIX& OutProjection, bool ModelSpace)
{
	// Identity matrix
	NiTransform temp;

	temp.m_Rotate.m_pEntry[0][0] = 0.0f;
	temp.m_Rotate.m_pEntry[0][1] = 1.0f;
	temp.m_Rotate.m_pEntry[0][2] = 0.0f;

	temp.m_Rotate.m_pEntry[1][0] = -1.0f;
	temp.m_Rotate.m_pEntry[1][1] = 0.0f;
	temp.m_Rotate.m_pEntry[1][2] = 0.0f;

	temp.m_Rotate.m_pEntry[2][0] = 0.0f;
	temp.m_Rotate.m_pEntry[2][1] = 0.0f;
	temp.m_Rotate.m_pEntry[2][2] = 1.0f;

	temp.m_Translate = BSGraphics::Renderer::QInstance()->GetRendererShadowState()->m_PosAdjust;
	temp.m_fScale = 1.0f;

	if (ModelSpace)
	{
		OutProjection = BSShaderUtil::GetXMFromNi(temp);
	}
	else
	{
		XMMATRIX m1 = BSShaderUtil::GetXMFromNi(temp);
		XMMATRIX m2 = BSShaderUtil::GetXMFromNi(ObjectWorldTrans);

		// OutProjection = Mul(Translate(m2, renderer->m_PosAdjust), m1);
		m2.r[3] = XMVectorAdd(m2.r[3], XMVectorSet(
			temp.m_Translate.x,
			temp.m_Translate.y,
			temp.m_Translate.z,
			0.0f));

		OutProjection = XMMatrixMultiply(m2, m1);
	}
}