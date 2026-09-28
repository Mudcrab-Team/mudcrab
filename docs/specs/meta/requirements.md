# Build and Conversion Requirements

What a contributor's machine spends on OpenSkyrim, measured on a Windows desktop with NVMe drives and
Skyrim Special Edition with its free Creation Club content. Treat the times as a guide: they depend on the
CPU, the disk and what else is running.

## Building

| Step | Time | Notes |
| :--- | :--- | :--- |
| `cargo check --workspace`, first time | about 5 min | compiles Bevy and every dependency once |
| `cargo build --release --bin engine`, first time | longer than a check | optimised code generation plus link-time optimisation |
| a one-file engine change, `--release` | about 4-5 min | mostly link-time optimisation |
| a one-file engine change, `--profile quick` | about 40 s | release without link-time optimisation (`cargo build --profile quick --bin engine`); not for measuring |
| the build folder (`target/`) | 20-30 GB | debug and release profiles together |

## Converting a Skyrim SE install

| Item | Size or time | Notes |
| :--- | :--- | :--- |
| the game's `Data` folder | about 15 GB | read only; the converter never writes into it |
| a first (fresh) conversion | about 5 hours | measured 5.1 h; most of it is encoding textures to Basis UASTC |
| a reconversion with unchanged inputs | about 30 minutes | converted assets are reused through `conversion-manifest.json` |
| the converted output | about 70 GB | textures about 19 GB, the rest mostly the extracted archives (`vfs/`, `.ingestion-cache/`) |
| free space during a reconversion | about the size of the output again | the new output is staged beside the old one and renamed over it at the end |

Without Skyrim, the `dummy-content` fixture (see [CONTRIBUTING](../../../CONTRIBUTING.md)) builds and
converts a small synthetic game in a couple of minutes.

## Running the engine

Any desktop GPU with Vulkan, DirectX 12 or Metal support runs the engine. The converted textures are
Basis UASTC KTX2, transcoded at load time to the GPU's compressed format.
