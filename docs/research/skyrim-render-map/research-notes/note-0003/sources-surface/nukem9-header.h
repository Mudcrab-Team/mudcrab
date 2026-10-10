#pragma once

#include "../BSShader.h"

class BSLightingShaderMaterialBase;
class BSLightingShaderMaterialLandscape;
class BSLightingShaderProperty;
class BSMultiIndexTriShape;

class BSLightingShader : public BSShader
{
private:
	enum Techniques
	{
		RAW_TECHNIQUE_NONE = 0,
		RAW_TECHNIQUE_ENVMAP = 1,
		RAW_TECHNIQUE_GLOWMAP = 2,
		RAW_TECHNIQUE_PARALLAX = 3,
		RAW_TECHNIQUE_FACEGEN = 4,
		RAW_TECHNIQUE_FACEGENRGBTINT = 5,
		RAW_TECHNIQUE_HAIR = 6,
		RAW_TECHNIQUE_PARALLAXOCC = 7,
		RAW_TECHNIQUE_MTLAND = 8,
		RAW_TECHNIQUE_LODLAND = 9,
		RAW_TECHNIQUE_SNOW = 10,
		RAW_TECHNIQUE_MULTILAYERPARALLAX = 11,
		RAW_TECHNIQUE_TREE = 12,
		RAW_TECHNIQUE_LODOBJ = 13,
		RAW_TECHNIQUE_MULTIINDEXTRISHAPESNOW = 14,
		RAW_TECHNIQUE_LODOBJHD = 15,
		RAW_TECHNIQUE_EYE = 16,
		RAW_TECHNIQUE_CLOUD = 17,
		RAW_TECHNIQUE_LODLANDNOISE = 18,
		RAW_TECHNIQUE_MTLANDLODBLEND = 19,
	};

	enum
	{
		RAW_FLAG_VC = 1 << 0,
		RAW_FLAG_SKINNED = 1 << 1,
		RAW_FLAG_MODELSPACENORMALS = 1 << 2,
		RAW_FLAG_LIGHTCOUNT1 = 1 << 3,			// Probably not used
		RAW_FLAG_LIGHTCOUNT2 = 1 << 4,			// ^
		RAW_FLAG_LIGHTCOUNT3 = 1 << 5,			// ^
		RAW_FLAG_LIGHTCOUNT4 = 1 << 6,			// ^
		RAW_FLAG_LIGHTCOUNT5 = 1 << 7,			// ^
		RAW_FLAG_LIGHTCOUNT6 = 1 << 8,			// ^
		RAW_FLAG_SPECULAR = 1 << 9,
		RAW_FLAG_SOFT_LIGHTING = 1 << 10,
		RAW_FLAG_RIM_LIGHTING = 1 << 11,
		RAW_FLAG_BACK_LIGHTING = 1 << 12,
		RAW_FLAG_SHADOW_DIR = 1 << 13,
		RAW_FLAG_DEFSHADOW = 1 << 14,
		RAW_FLAG_PROJECTED_UV = 1 << 15,
		RAW_FLAG_ANISO_LIGHTING = 1 << 16,
		RAW_FLAG_AMBIENT_SPECULAR = 1 << 17,
		RAW_FLAG_WORLD_MAP = 1 << 18,
		RAW_FLAG_BASE_OBJECT_IS_SNOW = 1 << 19,
		RAW_FLAG_DO_ALPHA_TEST = 1 << 20,
		RAW_FLAG_SNOW = 1 << 21,
		RAW_FLAG_CHARACTER_LIGHT = 1 << 22,
		RAW_FLAG_ADDITIONAL_ALPHA_MASK = 1 << 23,
	};

	enum class Space
	{
		World = 0,
		Model = 1,
	};

	const static uintptr_t OriginalVTableBase = 0x187FBD8;

public:
	inline AutoPtr(BSLightingShader *, pInstance, 0x3257C48);

	DECLARE_CONSTRUCTOR_HOOK(BSLightingShader);

	char _pad0[0x4];
	uint32_t m_CurrentRawTechnique;
	char _pad1[0x60];

	BSLightingShader();
	virtual ~BSLightingShader();
	virtual bool SetupTechnique(uint32_t Technique) override;						// Implemented
	virtual void RestoreTechnique(uint32_t Technique) override;						// Implemented
	virtual void SetupMaterial(BSShaderMaterial const *Material) override;			// Implemented
	virtual void RestoreMaterial(BSShaderMaterial const *Material) override;		// Nullsub
	virtual void SetupGeometry(BSRenderPass *Pass, uint32_t RenderFlags) override;	// Implemented
	virtual void RestoreGeometry(BSRenderPass *Pass, uint32_t RenderFlags) override;// Implemented

	void CreateAllShaders();

	static uint32_t GetRawTechnique(uint32_t Technique);
	static uint32_t GetVertexTechnique(uint32_t RawTechnique);
	static uint32_t GetPixelTechnique(uint32_t RawTechnique);

	static std::vector<std::pair<const char *, const char *>> GetSourceDefines(uint32_t Technique);
	static std::string GetTechniqueString(uint32_t Technique);

private:
	static void TechUpdateHighDetailRangeConstants(BSGraphics::VertexCGroup& VertexCG);
	static void TechUpdateFogConstants(BSGraphics::VertexCGroup& VertexCG, BSGraphics::PixelCGroup& PixelCG);

	static void MatSetEnvTexture(const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material);
	static void MatSetEnvMaskTexture(const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material);
	static void MatSetTextureSlot(int Slot, const NiSourceTexture *Texture, const BSLightingShaderMaterialBase *Material);
	static void MatSetMultiTextureLandOverrides(const BSLightingShaderMaterialLandscape *Material);
	static __int64 sub_141314170(__int64 a1);

	static void GeometrySetupConstantWorld(BSGraphics::VertexCGroup& VertexCG, const NiTransform& Transform, bool IsPreviousWorld, const NiPoint3 *PosAdjust);
	static void GeometrySetupConstantLandBlendParams(const BSGraphics::VertexCGroup& VertexCG, const NiPoint3& Translate, float OffsetX, float OffsetY);
	static void GeometrySetupConstantTreeParams(const BSGraphics::VertexCGroup& VertexCG, BSLightingShaderProperty *Property);
	static void GeometrySetupConstantDirectionalLight(const BSGraphics::PixelCGroup& PixelCG, const BSRenderPass *Pass, DirectX::XMMATRIX& InvWorld, Space RenderSpace);
	static void GeometrySetupConstantDirectionalAmbientLight(const BSGraphics::PixelCGroup& PixelCG, const NiMatrix3& ModelToWorld, Space RenderSpace);
	static void GeometrySetupEmitColorConstants(const BSGraphics::PixelCGroup& PixelCG, BSLightingShaderProperty *Property);
	static void GeometrySetupConstantPointLights(const BSGraphics::PixelCGroup& PixelCG, BSRenderPass *Pass, DirectX::XMMATRIX& Transform, uint32_t LightCount, uint32_t ShadowLightCount, float WorldScale, Space RenderSpace);
	static void GeometrySetupConstantProjectedUVData(const BSGraphics::PixelCGroup& PixelCG, BSMultiIndexTriShape *Geometry, BSLightingShaderProperty *Property, bool EnableProjectedNormals);
	static void GenerateProjectionMatrix(const NiTransform& ObjectWorldTrans, DirectX::XMMATRIX& OutProjection, bool ModelSpace);
};
static_assert(sizeof(BSLightingShader) == 0xF8);
static_assert_offset(BSLightingShader, m_CurrentRawTechnique, 0x94);