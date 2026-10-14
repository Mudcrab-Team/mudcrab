# Web source readback (2026-10-10)

These short notes preserve the browser-derived evidence used in this audit. They are not full page captures.

## Community Shaders release v1.9.1

URL: https://github.com/community-shaders/skyrim-community-shaders/releases/tag/v1.9.1

The official page lists v1.9.1, released 2026-09-24, and the feature audit lists Light Limit Fix 3-2-0. It does not mention a 16-shadow release change in the displayed notes. This absence does not prove no prerelease or later branch implemented it.

## Community Shaders prerelease lookup

URL: https://github.com/community-shaders/skyrim-community-shaders/git/ref/tags/v1.6.0-pr1941

GitHub REST lookup returned Not Found. This exact tag reference was not resolved. The NewReleases mirror cited by the supplied note remains secondary and was not captured.

## ENB EFFECT documentation

URL: https://enbdev.com/doc_skyrim_effect_en.htm

The author documentation says UseOriginalPostProcessing can use the vanilla game post-processing algorithm. The selected setting is version/config dependent. The complete page body and active preset were not captured.

## Community source revision

URL: https://github.com/community-shaders/skyrim-community-shaders/commit/b81bf11cf126f41aaa53d7318b61293677d9c0cc

GitHub API resolved current dev to b81bf11cf126f41aaa53d7318b61293677d9c0cc at 2026-10-10T12:55:39Z. The three preserved current-dev LLF files match the corresponding files in the d001d4de pin by SHA-256, including C++ CLUSTER_MAX_LIGHTS=128 and shader MAX_CLUSTER_LIGHTS=256.
