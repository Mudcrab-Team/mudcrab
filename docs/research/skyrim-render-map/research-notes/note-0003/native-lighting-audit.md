# Note N003: surface lighting and authored environment audit

This audit covers the lighting clauses in the opening summary, the sections “A forward renderer with one deferred trick: the shadow mask,” “BSLightingShader packs light counts into its permutation key,” “The per-pixel equation is gamma-space Lambert plus unnormalized Blinn-Phong,” and “Weathers and Lighting Templates feed the same shader constants,” plus the surface-lighting claims in the conclusion. It does not audit ImageSpace grading or the later mod-intervention sections.

The strongest corrections are narrower than the supplied summary suggests. The early Community Shaders reconstruction computes point-light falloff as `1 - saturate(distance / radius)^2`; Mudcrab’s current ordinary native shader uses the same expression. This challenges N002-L21’s linear/quadratic exponent model, but remains a reconstruction-to-project match until the target shader bytes and a selected draw establish the retail equation. Similarly, the serialized XCLL inheritance mask contains no DALC flag. That proves a record-layout fact, while runtime cell/template precedence remains open.

The weather schema supports four named color-time slots and stores separate DALC blocks for those slots. Target-native static notes also recover affine DALC row preparation and a default sunlight consumer. They do not identify the active weather, complete source precedence, effective time weights, or the values reaching a selected draw. “All inputs come from data records” therefore overstates the evidence.

The reconstruction describes useful term ordering, including emissive before albedo and environment contribution after diffuse response. The scoped target evidence does not connect those complete equations to a selected shipped shader. The broad “gamma space” claim is also unproven: native legacy DDS views are UNORM, and that format alone does not establish a physical linear-light interpretation. No visual tuning conclusion follows from this static audit.

## Material conflicts and limits

- N003-L09 challenges N002-L21’s attenuation equation. N003 keeps the retail arithmetic unresolved because the cited HLSL is a hand reconstruction and the current Mudcrab shader is project code.
- N003-L13 challenges the SSS certainty in N002-L08/N002-L24. The source reconstruction has soft-light terms, but this audit does not exclude a separate specialized skin path in target shader bytes.
- N003-L16 strengthens the affine DALC representation in N002-L20 with target-native static row-preparation notes and pinned structure declarations. Active cell/weather ownership remains open.
- N003-L03 finds that the seven-local/four-shadow limit appears in the decompilation and reconstruction, but N002’s target-native inventory does not prove that array declaration or count in a selected retail draw.

## Evidence boundary

Source declarations, reverse-engineered C++, and reconstructed HLSL are used as source-assisted evidence. They are not treated as Bethesda source or as proof of target shader bytes. Target-native claims cite the existing target-pinned input and shader maps. Mudcrab code citations describe the current project implementation only. No build, game run, native cold import, GPU capture, or visual tuning was performed.
