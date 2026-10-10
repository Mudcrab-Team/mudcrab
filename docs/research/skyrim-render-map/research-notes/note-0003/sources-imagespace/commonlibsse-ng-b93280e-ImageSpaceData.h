#pragma once

namespace RE
{
	struct ImageSpaceBaseData
	{
		struct HDR  // HNAM
		{
		public:
			// members
			float eyeAdaptSpeed;          // 00
			float bloomBlurRadius;        // 04
			float bloomThreshold;         // 08
			float bloomScale;             // 0C
			float receiveBloomThreshold;  // 10
			float white;                  // 14
			float sunlightScale;          // 18
			float skyScale;               // 1C
			float eyeAdaptStrength;       // 20
		};
		static_assert(sizeof(HDR) == 0x24);

		struct Cinematic  // CNAM
		{
		public:
			// members
			float saturation;  // 0
			float brightness;  // 4
			float contrast;    // 8
		};
		static_assert(sizeof(Cinematic) == 0xC);

		struct Tint  // TNAM
		{
		public:
			struct ColorF
			{
			public:
				// members
				float red;    // 0
				float green;  // 4
				float blue;   // 8
			};
			static_assert(sizeof(ColorF) == 0xC);

			// members
			float  amount;  // 00
			ColorF color;   // 04
		};
		static_assert(sizeof(Tint) == 0x10);

		struct DepthOfField  // DNAM
		{
		public:
			enum class SkyBlurRadius
			{
				kRadius0 = 16384,        // 0x4000
				kRadius1 = 16672,        // 0x4120
				kRadius2 = 16784,        // 0x4190
				kRadius3 = 16848,        // 0x41D0
				kRadius4 = 16904,        // 0x4208
				kRadius5 = 16936,        // 0x4228
				kRadius6 = 16968,        // 0x4248
				kRadius7 = 17000,        // 0x4268
				kNoSky_Radius0 = 16576,  // 0x40C0
				kNoSky_Radius1 = 16736,  // 0x4160
				kNoSky_Radius2 = 16816,  // 0x41B0
				kNoSky_Radius3 = 16880,  // 0x41F0
				kNoSky_Radius4 = 16920,  // 0x4218
				kNoSky_Radius5 = 16952,  // 0x4238
				kNoSky_Radius6 = 16984,  // 0x4258
				kNoSky_Radius7 = 17016   // 0x4278
			};

			// members
			float                                          strength;       // 00
			float                                          distance;       // 04
			float                                          range;          // 08
			std::uint16_t                                  flags;          // 0C
			stl::enumeration<SkyBlurRadius, std::uint16_t> skyBlurRadius;  // 0E
		};
		static_assert(sizeof(DepthOfField) == 0x10);

		// members
		HDR          hdr;           // 00 - HNAM
		Cinematic    cinematic;     // 24 - CNAM
		Tint         tint;          // 30 - TNAM
		DepthOfField depthOfField;  // 40 - DNAM
	};
	static_assert(sizeof(ImageSpaceBaseData) == 0x50);

	struct ImageSpaceModData
	{
	public:
		enum
		{
			kFadeAmount = 0,
			kFadeR = 1,
			kFadeG = 2,
			kFadeB = 3,
			kBlurRadius = 4,
			kDoubleVisionStrength = 5,
			kRadialBlurStrength = 6,
			kRadialBlurRampup = 7,
			kRadialBlurStart = 8,
			kRadialBlurRampdown = 9,
			kRadialBlurDownStart = 10,
			kRadialBlurCenterX = 11,
			kRadialBlurCenterY = 12,
			kDOFStrength = 13,
			kDOFDistance = 14,
			kDOFRange = 15,
			kDOFMode = 16,
			kMotionBlurStrength = 17,

			kTotal = 18
		};

		// members
		float data[kTotal];  // 00
	};
	static_assert(sizeof(ImageSpaceModData) == 0x48);

	class ImageSpaceData
	{
	public:
		// members
		ImageSpaceBaseData baseData;    // 00
		ImageSpaceModData  modData;     // 50
		float              baseAmount;  // 98
		float              modAmount;   // 9C
	};
	static_assert(sizeof(ImageSpaceData) == 0xA0);
}
