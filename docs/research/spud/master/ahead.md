# Where Mudcrab is ahead

> Written with an AI assistant; research notes, not checked line by line by a human. Read-only source review of [spud](https://github.com/chasmlol/spud) (MIT) at commit [`8c990420`](https://github.com/chasmlol/spud/commit/8c9904207a5c573a0193fb903fb5c433f127f102), 2026-11-13. Nothing was built or run. See the [README](../README.md) for evidence levels and the licence note.
> **Topic:** Where Mudcrab is ahead of spud.

- **Target-platform foundation.** The inventory describes Rust, Bevy, Metal and Windows targets. Spud's checked-in build targets Windows. This matters because macOS is a primary Mudcrab goal; platform runtime success remains a separate claim.
- **Automated test infrastructure.** Mudcrab has unit, integration, fixture and parity-oriented tests across crates. Spud's report found no conventional test framework or CI registration. Test attribute counts do not establish passing status or comparable coverage.
- **Synthetic retail-independent fixtures.** Mudcrab's dummy-content crate generates multiple game-format fixtures. These support repeatable regression tests without distributing game content. They do not replace checks against representative retail assets.
- **CI and contribution governance.** Mudcrab has GitHub Actions, ownership and an AI policy. Gate work remains open in #421 and receipt validation in #524. The Spud report found no checked-in CI workflow.
- **Managed conversion reuse and provenance.** Mudcrab ships reuse-assets, repair and provenance-aware packs. Spud also has incremental caching, so a broad claim that its cache is inferior would be unsupported. Mudcrab's explicit pack immutability and reuse workflow supports expensive conversions and concurrent work.
- **Certified cluster work is a draft, not a shipped advantage.** Mudcrab tracks derived cluster payloads and rendering in draft stack #427–#462, including #430, #439 and #441. Spud has GPU-driven submission; the reports do not establish a feature-equivalent comparison.

Source: [Mudcrab inventory](../mudcrab-inventory.md), [meta report](../wave1/meta.md), and the area atlases. Platform and performance claims require target-specific runtime evidence.
