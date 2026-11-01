//! Bind the DDS bytes a certified terrain bake actually read to the existing core.
//! This reads metadata only: no locking, copying, image decoding or publication.
//! The caller retains its dataset lock and supplies TerrainTextures::source_hashes
//! after decoding and before baking. Benchmarks do not acquire this certification.

use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    cache::CONVERTER_SCHEMA_VERSION,
};
use color_eyre::{Result, eyre::ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, fs, marker::PhantomData, path::Path};

pub(crate) struct TerrainSourceContract {
    expected: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct LodSources {
    build_identity: String,
    #[serde(deserialize_with = "unique_map")]
    terrain_sources: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ProducerSources {
    schema_version: u32,
    #[serde(deserialize_with = "unique_map")]
    entries: BTreeMap<String, SourceEntry>,
}

#[derive(Deserialize)]
struct SourceEntry {
    source_hash: String,
}

impl TerrainSourceContract {
    pub(crate) fn open(converted: &Path) -> Result<Self> {
        shared::world_assets::validate_lod_build_contract(converted, CONVERTER_SCHEMA_VERSION)?;
        // The shared startup contract allows a missing manifest for zero chunks;
        // certified atlas derivation needs the actual recorded source recipe.
        let lod: LodSources =
            serde_json::from_slice(&fs::read(converted.join("lod-manifest.json"))?)?;
        let connection = Connection::open_with_flags(
            converted.join("skyrim_world.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let core = shared::extensions::core_build_identity(&connection)?
            .filter(|identity| !identity.trim().is_empty())
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("terrain sources require a nonempty core identity")
            })?;
        ensure!(
            lod.build_identity == core,
            "terrain source manifest is stale for the current core"
        );
        let producer: ProducerSources =
            serde_json::from_slice(&fs::read(converted.join("conversion-manifest.json"))?)?;
        ensure!(
            producer.schema_version == CONVERTER_SCHEMA_VERSION,
            "terrain source producer schema differs"
        );
        for (path, expected) in &lod.terrain_sources {
            canonical_dds(path)?;
            canonical_hash(expected)?;
            let entry = producer.entries.get(path).ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "terrain source has no exact converted-source entry: {path}"
                )
            })?;
            ensure!(
                raw_hash(&entry.source_hash)? == expected,
                "terrain source recipe disagrees with conversion manifest: {path}"
            );
        }
        // A canonical entry plus an aliased spelling is not two independent
        // sources. Reject aliases of the recorded terrain domain explicitly.
        for path in producer.entries.keys() {
            // Let the existing texture parser decide the input domain before
            // checking its canonical spelling. Percent-encoded extensions and
            // trailing separators can normalize to a recorded DDS too. The
            // parser rejects other kinds' extensions (GLB/NIF/PEX), which this
            // terrain-only contract must leave to their respective owners.
            let Ok(canonical) = canonical_asset_path(path, AssetKind::Texture, "dds") else {
                continue;
            };
            if lod.terrain_sources.contains_key(&canonical) {
                ensure!(*path == canonical, "aliased terrain source entry: {path}");
            }
        }
        Ok(Self {
            expected: lod.terrain_sources,
        })
    }

    /// Validates only what this area/world actually consumed. Additional recorded
    /// sources are allowed; an unrecorded actual source is never certified.
    pub(crate) fn validate_actual(&self, actual: &BTreeMap<String, String>) -> Result<()> {
        for (path, hash) in actual {
            canonical_dds(path)?;
            canonical_hash(hash)?;
            let expected = self.expected.get(path).ok_or_else(|| {
                color_eyre::eyre::eyre!("terrain bake consumed an unrecorded DDS: {path}")
            })?;
            ensure!(
                hash == expected,
                "terrain bake consumed changed DDS bytes: {path}"
            );
        }
        Ok(())
    }

    pub(crate) fn sources(&self) -> impl Iterator<Item = (&str, &str)> {
        self.expected
            .iter()
            .map(|(path, hash)| (path.as_str(), hash.as_str()))
    }
}

fn canonical_dds(path: &str) -> Result<()> {
    ensure!(
        path.ends_with(".dds") && canonical_asset_path(path, AssetKind::Texture, "dds")? == path,
        "terrain source path is not a canonical DDS key: {path}"
    );
    Ok(())
}

fn canonical_hash(hash: &str) -> Result<()> {
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "terrain source hash is not canonical SHA-256"
    );
    Ok(())
}

/// The recorded input recipe begins with the raw source SHA, optionally followed
/// by colon-delimited conversion dependencies/encoding. Preserve that suffix as
/// opaque producer metadata; it is not part of the raw DDS content hash.
fn raw_hash(recipe: &str) -> Result<&str> {
    let raw = recipe.split_once(':').map_or(recipe, |(raw, _)| raw);
    canonical_hash(raw)?;
    Ok(raw)
}

/// serde's ordinary BTreeMap keeps the last duplicate key. A provenance map must
/// refuse duplicate paths rather than silently selecting an authored record.
fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map with unique source paths")
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((path, value)) = map.next_entry::<String, T>()? {
                if result.insert(path.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate source path: {path}"
                    )));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cache::hash_bytes, esm::exporter::create_tables};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    const A: &str = "textures/landscape/invented-a.dds";
    const B: &str = "textures/landscape/invented-b.dds";

    fn seed() -> TempDir {
        let directory = tempfile::tempdir().unwrap();
        let db = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        create_tables(&db).unwrap();
        db.execute(
            "INSERT INTO lod_build(id,build_identity) VALUES(1,?1)",
            ["ab".repeat(32)],
        )
        .unwrap();
        let sources = BTreeMap::from([
            (A, hash_bytes(b"first invented DDS bytes")),
            (B, hash_bytes(b"second invented DDS bytes")),
        ]);
        let entries: BTreeMap<_, _> = sources
            .iter()
            .map(|(path, hash)| {
                (
                    *path,
                    json!({"source_hash": format!("{hash}:texture-encoding:ColorSrgb")}),
                )
            })
            .collect();
        fs::write(
            directory.path().join("conversion-manifest.json"),
            serde_json::to_vec(&json!({
                "schema_version": CONVERTER_SCHEMA_VERSION, "entries": entries,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            directory.path().join("lod-manifest.json"),
            serde_json::to_vec(&json!({
                "build_identity": "ab".repeat(32), "converter_schema": CONVERTER_SCHEMA_VERSION,
                "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
                "chunks": 0, "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
                "terrain_sources": sources,
            }))
            .unwrap(),
        )
        .unwrap();
        directory
    }

    fn edit(root: &Path, file: &str, change: impl FnOnce(&mut Value)) {
        let path = root.join(file);
        let mut json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut json);
        fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
    }

    #[test]
    fn actual_loaded_hashes_can_be_a_strict_recorded_subset() {
        let fixture = seed();
        let contract = TerrainSourceContract::open(fixture.path()).unwrap();
        assert_eq!(contract.sources().count(), 2);
        contract
            .validate_actual(&BTreeMap::from([(
                A.into(),
                hash_bytes(b"first invented DDS bytes"),
            )]))
            .unwrap();
        contract.validate_actual(&BTreeMap::new()).unwrap();
    }

    #[test]
    fn changed_raw_bytes_and_new_actual_path_are_not_certified() {
        let fixture = seed();
        let contract = TerrainSourceContract::open(fixture.path()).unwrap();
        let changed = BTreeMap::from([(A.into(), hash_bytes(b"first invented DDS byte!"))]);
        assert!(
            contract
                .validate_actual(&changed)
                .unwrap_err()
                .to_string()
                .contains("changed DDS bytes")
        );
        let unrecorded = BTreeMap::from([(
            "textures/landscape/not-recorded.dds".into(),
            hash_bytes(b"first invented DDS bytes"),
        )]);
        assert!(
            contract
                .validate_actual(&unrecorded)
                .unwrap_err()
                .to_string()
                .contains("unrecorded DDS")
        );
    }

    #[test]
    fn actual_and_recorded_path_aliases_are_refused() {
        for alias in [
            "Textures/landscape/invented-a.dds",
            "textures\\landscape\\invented-a.dds",
            "textures/landscape/./invented-a.dds",
            "textures/landscape/invented-a%2Edds",
            "textures/landscape/invented-a.dds/",
        ] {
            let fixture = seed();
            let contract = TerrainSourceContract::open(fixture.path()).unwrap();
            assert!(
                contract
                    .validate_actual(&BTreeMap::from([(
                        alias.into(),
                        hash_bytes(b"first invented DDS bytes")
                    )]))
                    .is_err()
            );
            edit(fixture.path(), "lod-manifest.json", |json| {
                json["terrain_sources"][alias] = json["terrain_sources"][A].clone();
            });
            assert!(TerrainSourceContract::open(fixture.path()).is_err());
        }
        let fixture = seed();
        edit(fixture.path(), "conversion-manifest.json", |json| {
            json["entries"]["Textures/landscape/invented-a.dds"] = json["entries"][A].clone();
        });
        assert!(TerrainSourceContract::open(fixture.path()).is_err());
    }

    #[test]
    fn producer_texture_aliases_are_refused_before_or_after_canonical_entries() {
        for alias in [
            "Textures/landscape/invented-a.dds",
            "textures/landscape/invented-a%2Edds",
            "textures/landscape/invented-a.dds/",
        ] {
            // Establish that the existing texture parser actually aliases this
            // input, rather than obtaining an unrelated parse failure.
            assert_eq!(
                canonical_asset_path(alias, AssetKind::Texture, "dds").unwrap(),
                A
            );
            for alias_first in [true, false] {
                let fixture = seed();
                let path = fixture.path().join("conversion-manifest.json");
                let producer: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                let entry = &producer["entries"][A];
                let canonical_pair = format!("{}:{entry}", serde_json::to_string(A).unwrap());
                let alias_pair = format!("{}:{entry}", serde_json::to_string(alias).unwrap());
                let second_pair = format!(
                    "{}:{}",
                    serde_json::to_string(B).unwrap(),
                    producer["entries"][B]
                );
                // Construct the authored order explicitly; Value's map order
                // depends on serde_json's feature union and is not this proof.
                let pairs = if alias_first {
                    format!("{alias_pair},{canonical_pair},{second_pair}")
                } else {
                    format!("{canonical_pair},{second_pair},{alias_pair}")
                };
                fs::write(
                    path,
                    format!(
                        "{{\"schema_version\":{CONVERTER_SCHEMA_VERSION},\"entries\":{{{pairs}}}}}"
                    ),
                )
                .unwrap();
                let error = TerrainSourceContract::open(fixture.path())
                    .err()
                    .expect("producer alias must refuse");
                assert!(
                    error.to_string().contains("aliased terrain source entry"),
                    "wrong refusal for {alias}: {error:#}"
                );
            }
        }
    }

    #[test]
    fn unrelated_nontexture_inputs_do_not_become_dds_aliases() {
        let fixture = seed();
        edit(fixture.path(), "conversion-manifest.json", |json| {
            for path in [
                "textures/landscape/invented-a.glb",
                "textures/landscape/invented-a.nif",
                "meshes/unused.glb",
                "scripts/unused.pex",
                "unrelated.unsupported",
            ] {
                assert!(canonical_asset_path(path, AssetKind::Texture, "dds").is_err());
                json["entries"][path] = json!({"source_hash":"unrelated producer metadata"});
            }
        });
        let contract = TerrainSourceContract::open(fixture.path()).unwrap();
        contract
            .validate_actual(&BTreeMap::from([(
                A.into(),
                hash_bytes(b"first invented DDS bytes"),
            )]))
            .unwrap();
    }

    #[test]
    fn changed_or_missing_conversion_source_recipe_is_refused() {
        for change in ["changed", "missing", "malformed"] {
            let fixture = seed();
            edit(
                fixture.path(),
                "conversion-manifest.json",
                |json| match change {
                    "changed" => {
                        json["entries"][A]["source_hash"] =
                            json!(format!("{}:texture-encoding:ColorSrgb", "cd".repeat(32)))
                    }
                    "missing" => {
                        json["entries"].as_object_mut().unwrap().remove(A);
                    }
                    "malformed" => json["entries"][A]["source_hash"] = json!("not-a-source-hash"),
                    _ => unreachable!(),
                },
            );
            assert!(
                TerrainSourceContract::open(fixture.path()).is_err(),
                "case {change}"
            );
        }
    }

    #[test]
    fn missing_lod_manifest_and_stale_core_are_refused_even_with_zero_chunks() {
        let fixture = seed();
        fs::remove_file(fixture.path().join("lod-manifest.json")).unwrap();
        assert!(TerrainSourceContract::open(fixture.path()).is_err());
        let fixture = seed();
        Connection::open(fixture.path().join("skyrim_world.db"))
            .unwrap()
            .execute("UPDATE lod_build SET build_identity=?1", ["cd".repeat(32)])
            .unwrap();
        assert!(TerrainSourceContract::open(fixture.path()).is_err());
    }

    #[test]
    fn duplicate_authored_source_paths_and_noncanonical_hashes_are_refused() {
        let fixture = seed();
        let path = fixture.path().join("lod-manifest.json");
        let json = fs::read_to_string(&path).unwrap();
        let needle = "\"terrain_sources\":{";
        let duplicated = json.replacen(
            needle,
            &format!("{needle}\"{A}\":\"{}\",", "00".repeat(32)),
            1,
        );
        assert_ne!(json, duplicated);
        fs::write(path, duplicated).unwrap();
        assert!(TerrainSourceContract::open(fixture.path()).is_err());
        let fixture = seed();
        edit(fixture.path(), "lod-manifest.json", |json| {
            json["terrain_sources"][A] = json!("g0".repeat(32))
        });
        assert!(TerrainSourceContract::open(fixture.path()).is_err());
    }
}
