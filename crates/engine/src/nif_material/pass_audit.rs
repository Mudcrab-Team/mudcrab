//! The pass each converted material draws in under `skyrim`, against retail's rules (SPEC-1
//! sections 6.3, 8.1, 9.2 and 9.3, and SPEC-2 section 4.2;
//! [near-lighting.md](../../../../docs/specs/engine/near-lighting.md)).
//!
//! [`rule`] is the specification as a table, written apart from the loader. [`drawn`] is what the
//! loader does. A property test compares them over every flag combination, and the ignored test
//! `pack_pass_audit` compares them over every material of a converted pack:
//!
//! ```sh
//! MUDCRAB_PASS_AUDIT_PACK=<pack> MUDCRAB_PASS_AUDIT_REPORT=<report.md> \
//!   python3 scripts/workspace.py cargo test --locked -p engine --lib -- --ignored pack_pass_audit
//! ```
//!
//! A rule the specification leaves open carries a [`Open`] row. The table then states what the
//! engine does, so no case has a silent default.

use super::{
    NativeMaterialHandler, has_external_emittance, has_model_space_normals,
    is_additive_emittance_decal, native_depth_flags, palette_lookup, retail,
};
use ::gltf::Material;
use bevy::prelude::*;
use shared::nif_alpha::NifAlphaProperty;
use std::collections::{BTreeMap, BTreeSet};

/// Property flags of the first word.
const REFRACTION: u32 = 1 << 15;
const TEMP_REFRACTION: u32 = 1 << 2;
const DECAL: u32 = 1 << 26;
const DYNAMIC_DECAL: u32 = 1 << 27;
const DEPTH_TEST: u32 = 1 << 31;
/// Property flags of the second word: flag `n` is bit `n - 32`.
const DEPTH_WRITE: u32 = 1;
const LOD_LANDSCAPE: u32 = 1 << 1;
const LOD_OBJECTS: u32 = 1 << 2;
const TWO_SIDED: u32 = 1 << 4;
const SPECULAR_BIT_41: u32 = 1 << 9;

/// A rule the specification does not give, or gives only in part. Each is a stand-in row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Open {
    OtherFamily,
    LodTechnique,
    Refraction,
    NoDepthTest,
    EffectNotCovered,
    OpaqueDepth,
    SortedDepth,
    BlendPair,
    TwoSided,
    DecalAlpha,
    DecalTestWithBit41,
    DecalPrepass,
}

impl Open {
    const fn label(self) -> &'static str {
        match self {
            Self::OtherFamily => {
                "STAND-IN(#362): water, sky and other shader families keep their own passes"
            }
            Self::LodTechnique => "STAND-IN(#946): LOD techniques are outside the near rules",
            Self::Refraction => {
                "STAND-IN(#362): a refraction surface has no Lighting pass and draws as a surface"
            }
            Self::NoDepthTest => {
                "STAND-IN(#362): the pass of a surface without a depth test is not specified"
            }
            Self::EffectNotCovered => {
                "STAND-IN(#362): the effect shader's pass is specified only for a plain blended surface"
            }
            Self::OpaqueDepth => {
                "STAND-IN(#362): depth write of an unblended surface follows its flag"
            }
            Self::SortedDepth => "STAND-IN(#362): a sorted blended draw writes no depth",
            Self::BlendPair => {
                "STAND-IN(#362): a blend pair outside the table draws as a plain alpha blend"
            }
            Self::TwoSided => "STAND-IN(#362): the two-sided flag does not change the pass",
            Self::DecalAlpha => {
                "STAND-IN(#362): a decal with property alpha below one and no blend is not specified"
            }
            Self::DecalTestWithBit41 => {
                "STAND-IN(#362): the alpha cut of a flag-41 decal depends on the unknown prepass"
            }
            Self::DecalPrepass => "STAND-IN(#362): a decal stays out of the depth prepass",
        }
    }
}

/// A rule the specification gives and the engine does not follow. Each is a stand-in row, and the
/// pack audit counts its materials as mismatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Gap {
    SortedDecal,
    CutoutWithBlend,
    BlendWithoutCut,
}

impl Gap {
    const fn label(self) -> &'static str {
        match self {
            Self::SortedDecal => {
                "STAND-IN(#362): a blended decal draws after the composite and applies it in its shader"
            }
            Self::CutoutWithBlend => {
                "STAND-IN(#362): a blended surface with an alpha test and depth write draws as a cutout"
            }
            Self::BlendWithoutCut => {
                "STAND-IN(#362): a blended surface with an alpha test and no depth write is not cut"
            }
        }
    }
}

/// What the engine draws where it does not follow `retail`.
fn known_gap(authored: &Authored, retail: &Pass) -> Option<(Gap, Pass)> {
    if retail.shade != Shade::Blend {
        return None;
    }
    let decal = authored.flags_1 & (DECAL | DYNAMIC_DECAL) != 0;
    let zwrite = authored.flags_2 & DEPTH_WRITE != 0;
    if retail.alpha_test && (decal || zwrite) {
        // SPEC-2 §4.2 selects blend state only after the cutout has been routed.
        let pass = Pass {
            stage: Stage::First,
            shade: Shade::Cutout,
            alpha_test: true,
            depth_write: true,
            blend: None,
        };
        return Some((Gap::CutoutWithBlend, pass));
    }
    if retail.alpha_test {
        let pass = Pass {
            alpha_test: false,
            ..retail.clone()
        };
        return Some((Gap::BlendWithoutCut, pass));
    }
    decal.then(|| {
        let pass = Pass {
            stage: Stage::Sorted,
            depth_write: zwrite,
            ..retail.clone()
        };
        (Gap::SortedDecal, pass)
    })
}

/// Before the composite (the first stage) or in the sorted draws after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    First,
    Sorted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shade {
    Opaque,
    Cutout,
    Blend,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Pass {
    stage: Stage,
    shade: Shade,
    alpha_test: bool,
    depth_write: bool,
    /// The factors of a blended draw.
    blend: Option<[u8; 2]>,
}

/// What a material authors, as the converter publishes it.
#[derive(Clone, Debug)]
struct Authored {
    lighting: bool,
    effect: bool,
    flags_1: u32,
    flags_2: u32,
    alpha: Option<NifAlphaProperty>,
    property_alpha: f32,
}

/// The retail pass of `authored`, with the open rules it rests on. `None` for a material this
/// table does not judge.
fn rule(authored: &Authored) -> (Option<Pass>, BTreeSet<Open>) {
    let mut open = BTreeSet::new();
    let (flags_1, flags_2) = (authored.flags_1, authored.flags_2);
    if !authored.lighting && !authored.effect {
        open.insert(Open::OtherFamily);
        return (None, open);
    }
    if authored.lighting && flags_2 & (LOD_LANDSCAPE | LOD_OBJECTS) != 0 {
        open.insert(Open::LodTechnique);
        return (None, open);
    }
    if flags_1 & (REFRACTION | TEMP_REFRACTION) != 0 {
        open.insert(Open::Refraction);
        return (None, open);
    }
    if flags_1 & DEPTH_TEST == 0 {
        open.insert(Open::NoDepthTest);
        return (None, open);
    }
    if flags_2 & TWO_SIDED != 0 {
        open.insert(Open::TwoSided);
    }
    let blend_bit = authored.alpha.is_some_and(|alpha| alpha.blend_enabled);
    let test = authored.alpha.is_some_and(|alpha| alpha.test_enabled);
    let decal = flags_1 & (DECAL | DYNAMIC_DECAL) != 0;
    let mut factors = || {
        let Some(alpha) = authored.alpha.filter(|alpha| alpha.blend_enabled) else {
            return [6, 7];
        };
        match (alpha.source_factor, alpha.destination_factor) {
            (6, 7) => [6, 7],
            (6, 0) | (0, 0) | (6, 9) => [6, 0],
            (4, 7) => [4, 7],
            (1, 2) | (4, 1) => [4, 1],
            _ => {
                open.insert(Open::BlendPair);
                [6, 7]
            }
        }
    };
    let unblended = if test { Shade::Cutout } else { Shade::Opaque };
    // The effect shader is outside SPEC-1 section 9. SPEC-2 section 4.3 puts a blended effect
    // draw in the sorted list; it says nothing about an effect decal or an effect alpha test.
    if !authored.lighting && (decal || test || !blend_bit) {
        open.insert(Open::EffectNotCovered);
        return (None, open);
    }
    // SPEC-1 section 9.2: a surface that is not a decal also blends when its property alpha is
    // below one.
    let blended = blend_bit || (!decal && authored.property_alpha < 1.0);
    if blended {
        let stage = if decal { Stage::First } else { Stage::Sorted };
        let pass = Pass {
            stage,
            shade: Shade::Blend,
            alpha_test: test,
            depth_write: false,
            blend: Some(factors()),
        };
        if decal {
            open.insert(Open::DecalPrepass);
        } else {
            open.insert(Open::SortedDepth);
        }
        return (Some(pass), open);
    }
    let depth_write = if decal {
        open.insert(Open::DecalPrepass);
        if authored.property_alpha < 1.0 {
            open.insert(Open::DecalAlpha);
        }
        if test && flags_2 & SPECULAR_BIT_41 != 0 {
            open.insert(Open::DecalTestWithBit41);
        }
        true
    } else {
        open.insert(Open::OpaqueDepth);
        flags_2 & DEPTH_WRITE != 0
    };
    let pass = Pass {
        stage: Stage::First,
        shade: unblended,
        alpha_test: test,
        depth_write,
        blend: None,
    };
    (Some(pass), open)
}

/// What the `skyrim` loader draws `source` as.
fn drawn(handler: &NativeMaterialHandler, source: &Material<'_>) -> Pass {
    // The stock glTF material, then the loader's own two steps.
    let stock = StandardMaterial {
        alpha_mode: drawn_alpha_mode(source),
        base_color: Color::linear_rgba(
            1.0,
            1.0,
            1.0,
            source.pbr_metallic_roughness().base_color_factor()[3],
        ),
        ..default()
    };
    let mut material = stock.clone();
    if retail::is_lighting(source) {
        retail::apply(source, &stock, &mut material);
    }
    let loaded = material.alpha_mode;
    let state = handler.depth_state(source, loaded);
    let shade = match loaded {
        AlphaMode::Blend => Shade::Blend,
        AlphaMode::Mask(_) => Shade::Cutout,
        _ => Shade::Opaque,
    };
    let sorted = match state {
        Some(state) => !state.first_stage,
        None => shade == Shade::Blend,
    };
    Pass {
        stage: if sorted { Stage::Sorted } else { Stage::First },
        shade,
        alpha_test: shade == Shade::Cutout,
        depth_write: state.map_or(shade != Shade::Blend, |state| state.depth_write),
        blend: (shade == Shade::Blend).then(|| {
            state
                .and_then(|state| state.blend_factors)
                .unwrap_or([6, 7])
        }),
    }
}

/// `None` when the material has no published shader flags.
fn authored(source: &Material<'_>) -> Option<Authored> {
    let (flags_1, flags_2) = native_depth_flags(source)?;
    let extras: serde_json::Value = serde_json::from_str(source.extras().as_ref()?.get()).ok()?;
    let family = extras.pointer("/openSkyrim/shaderFamily")?.as_str()?;
    let alpha = extras
        .pointer("/openSkyrim/alphaProperty")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    Some(Authored {
        lighting: family == "lighting",
        effect: family == "effect",
        flags_1,
        flags_2,
        alpha,
        property_alpha: source.pbr_metallic_roughness().base_color_factor()[3],
    })
}

fn skyrim() -> NativeMaterialHandler {
    NativeMaterialHandler {
        raw_colour_texels: true,
        ..default()
    }
}

/// A material as the converter writes it. The alpha mode follows the converter's rule: the test
/// bit gives `MASK`, else the blend bit gives `BLEND`, else `OPAQUE`.
fn published(authored: &Authored) -> serde_json::Value {
    let alpha_mode = match authored.alpha {
        Some(alpha) if alpha.test_enabled => "MASK",
        Some(alpha) if alpha.blend_enabled => "BLEND",
        _ => "OPAQUE",
    };
    serde_json::json!({
        "alphaMode": alpha_mode,
        "doubleSided": authored.flags_2 & TWO_SIDED != 0,
        "pbrMetallicRoughness": {"baseColorFactor": [1.0, 1.0, 1.0, authored.property_alpha]},
        "extras": {"openSkyrim": {
            "shaderFamily": if authored.lighting { "lighting" } else { "effect" },
            "shaderFlags1": authored.flags_1,
            "shaderFlags2": authored.flags_2,
            "alphaProperty": authored.alpha,
        }},
    })
}

fn parsed(material: &serde_json::Value) -> ::gltf::Gltf {
    let document = serde_json::json!({"asset": {"version": "2.0"}, "materials": [material]});
    ::gltf::Gltf::from_slice(document.to_string().as_bytes()).unwrap()
}

/// Every combination of the flags and alpha properties that select a pass.
fn combinations() -> Vec<Authored> {
    let mut alphas = vec![None];
    for blend_enabled in [false, true] {
        for test_enabled in [false, true] {
            for (source_factor, destination_factor) in [
                (6, 7),
                (6, 0),
                (0, 0),
                (6, 9),
                (4, 7),
                (1, 2),
                (4, 1),
                (2, 3),
            ] {
                alphas.push(Some(NifAlphaProperty {
                    blend_enabled,
                    source_factor,
                    destination_factor,
                    test_enabled,
                    test_function: 4,
                    threshold: 128,
                }));
            }
        }
    }
    let first_word = [DECAL, DYNAMIC_DECAL, DEPTH_TEST, REFRACTION];
    let second_word = [DEPTH_WRITE, TWO_SIDED, SPECULAR_BIT_41, LOD_OBJECTS];
    let pick = |bits: &[u32], mask: usize| {
        bits.iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .fold(0, |word, (_, bit)| word | bit)
    };
    let mut all = Vec::new();
    for lighting in [true, false] {
        for mask_1 in 0..16 {
            for mask_2 in 0..16 {
                for alpha in &alphas {
                    for property_alpha in [1.0, 0.5] {
                        all.push(Authored {
                            lighting,
                            effect: !lighting,
                            flags_1: pick(&first_word, mask_1),
                            flags_2: pick(&second_word, mask_2),
                            alpha: *alpha,
                            property_alpha,
                        });
                    }
                }
            }
        }
    }
    all
}

/// Whole random flag words, from a fixed seed.
fn random_samples(count: usize) -> Vec<Authored> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let combinations = combinations();
    (0..count)
        .map(|_| {
            let words = next().to_le_bytes();
            let mut sample =
                combinations[usize::try_from(next() % combinations.len() as u64).unwrap()].clone();
            sample.flags_1 = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
            sample.flags_2 = u32::from_le_bytes([words[4], words[5], words[6], words[7]]);
            // Half of the samples keep a depth test and clear the flags that leave a surface
            // unjudged, so the rules past them are reached.
            if next() & 1 == 0 {
                sample.flags_1 |= DEPTH_TEST;
                sample.flags_1 &= !(REFRACTION | TEMP_REFRACTION);
                sample.flags_2 &= !(LOD_LANDSCAPE | LOD_OBJECTS);
            }
            sample
        })
        .collect()
}

#[test]
fn the_loader_draws_every_flag_combination_in_the_specified_pass() {
    let handler = skyrim();
    let mut judged = 0_usize;
    let mut opened = BTreeSet::new();
    let mut gapped = BTreeSet::new();
    for sample in combinations().into_iter().chain(random_samples(20_000)) {
        let (expected, open) = rule(&sample);
        opened.extend(open);
        let Some(expected) = expected else {
            continue;
        };
        let gap = known_gap(&sample, &expected);
        gapped.extend(gap.as_ref().map(|(gap, _)| *gap));
        let expected = gap.map_or(expected, |(_, pass)| pass);
        let document = parsed(&published(&sample));
        let source = document.materials().next().unwrap();
        assert_eq!(drawn(&handler, &source), expected, "{sample:?}");
        judged += 1;
    }
    assert!(judged > 5_000, "{judged}");
    // Every open rule is reached, and each is a stand-in row of the near-lighting document.
    let document = include_str!("../../../../docs/specs/engine/near-lighting.md");
    for open in [
        Open::OtherFamily,
        Open::LodTechnique,
        Open::Refraction,
        Open::NoDepthTest,
        Open::EffectNotCovered,
        Open::OpaqueDepth,
        Open::SortedDepth,
        Open::BlendPair,
        Open::TwoSided,
        Open::DecalAlpha,
        Open::DecalTestWithBit41,
        Open::DecalPrepass,
    ] {
        assert!(
            opened.contains(&open) || open == Open::OtherFamily,
            "{open:?} is never reached"
        );
        assert!(
            document.contains(open.label()),
            "near-lighting.md lacks the row {}",
            open.label()
        );
    }
    for gap in [Gap::SortedDecal, Gap::CutoutWithBlend, Gap::BlendWithoutCut] {
        assert!(gapped.contains(&gap), "{gap:?} is never reached");
        assert!(document.contains(gap.label()), "{}", gap.label());
    }
}

/// Why [`Gap::SortedDecal`] changes no pixel of a plain alpha blend: the composite over a
/// blend equals the blend of the composite over the receiver with the sorted output of the decal,
/// at one fog factor.
#[test]
fn a_sorted_alpha_blend_equals_a_first_stage_blend_under_the_composite() {
    let k = crate::retail_lighting::OUTPUT_SCALE;
    let (receiver, decal) = (Vec3::new(0.1, 0.5, 0.9), Vec3::new(0.8, 0.3, 0.05));
    let fog_colour = Vec3::new(0.35, 0.45, 0.6);
    let composite = |colour: Vec3, factor: f32| {
        Vec3::from_array(std::array::from_fn(|channel| {
            crate::retail_fog::composite(colour[channel], 1.0, factor, fog_colour[channel], k)
        }))
    };
    for factor in [0.0, 0.4, 1.0] {
        for alpha in [0.0, 0.25, 1.0] {
            let first = composite(receiver.lerp(decal, alpha), factor);
            let sorted_decal =
                crate::retail_lighting::output_tail(decal, None, factor, fog_colour, 1.0);
            let sorted = composite(receiver, factor).lerp(sorted_decal, alpha);
            assert!(first.abs_diff_eq(sorted, 1.0e-6), "{first} {sorted}");
        }
    }
}

fn glb_files(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            glb_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
        {
            files.push(path);
        }
    }
}

#[derive(Default)]
struct Class {
    materials: usize,
    examples: Vec<String>,
}

impl Class {
    fn add(&mut self, mesh: &str) {
        self.materials += 1;
        if self.examples.len() < 3 && !self.examples.iter().any(|known| known == mesh) {
            self.examples.push(mesh.to_owned());
        }
    }

    fn row(&self, name: &str) -> String {
        format!(
            "| {name} | {} | {} |\n",
            self.materials,
            self.examples.join("<br>")
        )
    }
}

/// How a material with `External_Emittance` takes its reference's source colour
/// (`crate::emittance`). The colour itself is `byte / 255` on every path that takes it.
fn emittance_path(handler: &NativeMaterialHandler, source: &Material<'_>) -> &'static str {
    let state = handler.depth_state(source, drawn_alpha_mode(source));
    if palette_lookup(source).is_some() {
        "STAND-IN(#984): a palette effect does not take the source colour"
    } else if has_model_space_normals(source) {
        "STAND-IN(#362): a model-space-normal surface does not take the source colour"
    } else if retail::is_lighting(source) {
        "lit surface: the source replaces the emissive colour (SPEC-1 section 2.7)"
    } else if state.is_some_and(|state| is_additive_emittance_decal(source, state)) {
        "STAND-IN(#984): an additive effect decal multiplies its emission by the source, unfogged"
    } else {
        "STAND-IN(#984): another effect surface multiplies its emission by the source"
    }
}

fn drawn_alpha_mode(source: &Material<'_>) -> AlphaMode {
    match source.alpha_mode() {
        ::gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
        ::gltf::material::AlphaMode::Mask => AlphaMode::Mask(source.alpha_cutoff().unwrap_or(0.5)),
        ::gltf::material::AlphaMode::Blend => AlphaMode::Blend,
    }
}

/// Reads every mesh of a converted pack. It writes nothing into the pack.
#[test]
#[ignore = "needs a converted pack: set MUDCRAB_PASS_AUDIT_PACK and MUDCRAB_PASS_AUDIT_REPORT"]
fn pack_pass_audit() {
    let pack = std::path::PathBuf::from(std::env::var("MUDCRAB_PASS_AUDIT_PACK").unwrap());
    let report = std::env::var("MUDCRAB_PASS_AUDIT_REPORT").unwrap();
    let meshes = pack.join("meshes");
    let mut files = Vec::new();
    glb_files(&meshes, &mut files);
    files.sort();
    let handler = skyrim();
    let (mut judged, mut matched, mut unreadable, mut unpublished) = (0, 0, 0, 0);
    let mut mismatches = BTreeMap::<String, Class>::new();
    let mut opens = BTreeMap::<Open, Class>::new();
    let mut gaps = BTreeMap::<Gap, Class>::new();
    let mut unexplained = 0;
    let mut emittance = BTreeMap::<&str, Class>::new();
    for file in &files {
        let name = file.strip_prefix(&meshes).unwrap().display().to_string();
        // silent-ok: the count of unreadable meshes is in the report.
        let Ok(document) = std::fs::read(file)
            .map_err(|_| ())
            .and_then(|bytes| ::gltf::Gltf::from_slice_without_validation(&bytes).map_err(|_| ()))
        else {
            unreadable += 1;
            continue;
        };
        for source in document.materials() {
            if has_external_emittance(&source) {
                emittance
                    .entry(emittance_path(&handler, &source))
                    .or_default()
                    .add(&name);
            }
            let Some(authored) = authored(&source) else {
                unpublished += 1;
                continue;
            };
            let (expected, open) = rule(&authored);
            for open in open {
                opens.entry(open).or_default().add(&name);
            }
            let Some(expected) = expected else {
                continue;
            };
            judged += 1;
            let found = drawn(&handler, &source);
            if found == expected {
                matched += 1;
            } else {
                match known_gap(&authored, &expected) {
                    Some((gap, pass)) if pass == found => {
                        gaps.entry(gap).or_default().add(&name);
                    }
                    _ => unexplained += 1,
                }
                let family = if authored.lighting {
                    "lighting"
                } else {
                    "effect"
                };
                let decal = if authored.flags_1 & (DECAL | DYNAMIC_DECAL) != 0 {
                    "decal"
                } else {
                    "surface"
                };
                mismatches
                    .entry(format!(
                        "{family} {decal}: rule `{expected:?}`, drawn `{found:?}`"
                    ))
                    .or_default()
                    .add(&name);
            }
        }
    }
    let mut text = format!(
        "# Pass audit\n\nPack: `{}`. Meshes: {}. Unreadable meshes: {unreadable}. Materials without \
         published shader flags: {unpublished}.\n\nMaterials judged by a rule: {judged}. Matching: \
         {matched}. Mismatching: {}. Mismatching outside the known gaps: {unexplained}.\n\n\
         ## Known gaps (stand-ins)\n\n| Stand-in | Materials | Examples |\n| --- | --- | --- |\n",
        pack.display(),
        files.len(),
        judged - matched,
    );
    for (gap, class) in &gaps {
        text += &class.row(gap.label());
    }
    text += "\n## Mismatch classes\n\n| Class | Materials | Examples |\n| --- | --- | --- |\n";
    for (name, class) in &mismatches {
        text += &class.row(name);
    }
    text += "\n## Open rules (stand-ins)\n\nA material can count in more than one row.\n\n\
             | Stand-in | Materials | Examples |\n| --- | --- | --- |\n";
    for (open, class) in &opens {
        text += &class.row(open.label());
    }
    text += "\n## External emittance\n\nEvery material with the flag, by the path that gives it its \
             reference's colour.\n\n| Path | Materials | Examples |\n| --- | --- | --- |\n";
    for (path, class) in &emittance {
        text += &class.row(path);
    }
    std::fs::write(&report, text).unwrap();
    println!("judged {judged}, matched {matched}, unexplained {unexplained}, report {report}");
    assert_eq!(unexplained, 0, "see {report}");
}
