//! Native VMAD layout, bounded-corruption and publication regressions.
use converter::records::{Value, vmad};
use dummy_content::inhouse_vmad as fixture;

/// Select one authored member without using production parsing helpers.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct for {name}: {value:?}")
    };
    &members
        .iter()
        .find(|(key, _)| key == name)
        .unwrap_or_else(|| panic!("missing {name}: {value:?}"))
        .1
}

/// Select a physical array member without normalizing or sorting its order.
fn element(value: &Value, index: usize) -> &Value {
    let Value::Array(elements) = value else {
        panic!("expected array: {value:?}")
    };
    &elements[index]
}

/// Independently emit one scalar or array property for every declared VMAD type.
fn properties(format: i16) -> Vec<Vec<u8>> {
    let scalar_bytes = [
        Vec::new(),
        fixture::object(format, 0x02001731, -1, 0xBC51),
        fixture::text("Unequal é and 猫"),
        (-173189i32).to_le_bytes().to_vec(),
        1.73125f32.to_le_bytes().to_vec(),
        vec![1],
    ];
    let mut properties = Vec::new();
    for (kind, bytes) in scalar_bytes.iter().enumerate() {
        properties.push(fixture::property(
            5,
            &format!("Scalar{kind}"),
            kind as u8,
            1,
            bytes,
        ));
    }
    let second_bytes = [
        Vec::new(),
        fixture::object(format, 0x02002957, 31, 0xD937),
        fixture::text("Second string λ"),
        93117i32.to_le_bytes().to_vec(),
        (-27.375f32).to_le_bytes().to_vec(),
        vec![0],
    ];
    for kind in 1..=5 {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&scalar_bytes[kind]);
        bytes.extend_from_slice(&second_bytes[kind]);
        properties.push(fixture::property(
            5,
            &format!("Array{kind}"),
            kind as u8 + 10,
            3,
            &bytes,
        ));
    }
    properties.push(fixture::property(
        5,
        "EmptyObjects",
        11,
        1,
        &0u32.to_le_bytes(),
    ));
    properties
}

/// Every primitive, array and object format retains values, counts, aliases and padding.
#[test]
fn all_property_types_and_both_object_formats_are_typed_and_canonical() {
    for format in [1, 2] {
        let bytes = fixture::field(
            5,
            format,
            &[fixture::script(5, "PropertyScript", 2, &properties(format))],
            &[],
        );
        let (value, canonical) = vmad::decode(&bytes, b"NPC_", |id| Ok(id ^ 0x03000000)).unwrap();
        let script = element(member(&value, "scripts"), 0);
        assert_eq!(member(script, "flags"), &Value::Unsigned(2));
        let props = member(script, "properties");
        assert_eq!(
            member(element(props, 0), "value"),
            &Value::Struct(Vec::new())
        );
        let object = member(element(props, 1), "value");
        assert_eq!(member(object, "form_id"), &Value::FormId(0x01001731));
        assert_eq!(member(object, "alias"), &Value::Signed(-1));
        assert_eq!(member(object, "unused"), &Value::Unsigned(0xBC51));
        assert_eq!(
            member(element(props, 2), "value"),
            &Value::String("Unequal é and 猫".into())
        );
        assert_eq!(member(element(props, 3), "value"), &Value::Signed(-173189));
        assert_eq!(member(element(props, 4), "value"), &Value::Float(1.73125));
        assert_eq!(member(element(props, 5), "value"), &Value::Unsigned(1));
        for index in 6..11 {
            assert_eq!(
                member(element(props, index), "array_count"),
                &Value::Unsigned(2)
            );
        }
        let object_array = member(element(props, 6), "value");
        assert_eq!(
            member(element(object_array, 0), "form_id"),
            &Value::FormId(0x01001731)
        );
        assert_eq!(
            member(element(object_array, 1), "form_id"),
            &Value::FormId(0x01002957)
        );
        assert_eq!(
            member(element(object_array, 1), "alias"),
            &Value::Signed(31)
        );
        assert_eq!(
            member(element(object_array, 1), "unused"),
            &Value::Unsigned(0xD937)
        );
        for (index, first, second) in [
            (
                7,
                Value::String("Unequal é and 猫".into()),
                Value::String("Second string λ".into()),
            ),
            (8, Value::Signed(-173189), Value::Signed(93117)),
            (9, Value::Float(1.73125), Value::Float(-27.375)),
            (10, Value::Unsigned(1), Value::Unsigned(0)),
        ] {
            assert_eq!(
                member(element(props, index), "value"),
                &Value::Array(vec![first, second])
            );
        }
        assert_eq!(
            member(element(props, 11), "value"),
            &Value::Array(Vec::new())
        );
        assert_eq!(canonical.len(), bytes.len());
        let (again, twice) = vmad::decode(&canonical, b"NPC_", Ok).unwrap();
        assert_eq!(again, value);
        assert_eq!(twice, canonical);
        assert_ne!(canonical, bytes);
        let differences: Vec<_> = bytes
            .iter()
            .zip(&canonical)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .collect();
        assert_eq!(
            differences.len(),
            3,
            "only three object high bytes are rewritten"
        );
    }
}

/// Old script/property versions omit flag bytes and never consume the following count.
#[test]
fn version_three_omits_flags_and_preserves_signed_property() {
    let props = [fixture::property(
        3,
        "OldValue",
        3,
        99,
        &(-21973i32).to_le_bytes(),
    )];
    let bytes = fixture::field(3, 1, &[fixture::script(3, "OldScript", 99, &props)], &[]);
    let (value, canonical) = vmad::decode(&bytes, b"WEAP", Ok).unwrap();
    let script = element(member(&value, "scripts"), 0);
    let prop = element(member(script, "properties"), 0);
    assert_eq!(member(prop, "value"), &Value::Signed(-21973));
    let Value::Struct(script_members) = script else {
        unreachable!()
    };
    let Value::Struct(prop_members) = prop else {
        unreachable!()
    };
    assert!(script_members.iter().all(|(name, _)| name != "flags"));
    assert!(prop_members.iter().all(|(name, _)| name != "flags"));
    assert_eq!(canonical, bytes);
}

/// Sparse event bits select their actual events instead of their ordinal list positions.
#[test]
fn info_and_package_fragments_preserve_sparse_event_order() {
    for (owner, flags) in [(b"INFO", 2), (b"PACK", 5)] {
        let entries = if flags == 2 {
            vec![fixture::named_fragment(-7, "EndScript", "End")]
        } else {
            vec![
                fixture::named_fragment(-13, "BeginScript", "Begin"),
                fixture::named_fragment(-19, "ChangeScript", "Change"),
            ]
        };
        let bytes = fixture::field(5, 2, &[], &fixture::events(flags, &entries));
        let (value, canonical) = vmad::decode(&bytes, owner, Ok).unwrap();
        let events = member(member(&value, "fragments"), "events");
        assert_eq!(
            member(element(events, 0), "event"),
            &Value::Unsigned(if flags == 2 { 2 } else { 1 })
        );
        if flags == 5 {
            assert_eq!(member(element(events, 1), "event"), &Value::Unsigned(4));
        }
        assert_eq!(canonical, bytes);
    }
}

/// Scene phase fields use the source's byte index and separate signed unknown values.
#[test]
fn scene_phase_header_fields_are_distinct() {
    let bytes = fixture::field(5, 2, &[], &fixture::scene_tail());
    let (value, canonical) = vmad::decode(&bytes, b"SCEN", Ok).unwrap();
    let phase = element(member(member(&value, "fragments"), "phases"), 0);
    assert_eq!(member(phase, "flags"), &Value::Unsigned(2));
    assert_eq!(member(phase, "phase_index"), &Value::Unsigned(173));
    assert_eq!(member(phase, "unknown_word"), &Value::Signed(-21917));
    assert_eq!(member(phase, "unknown_byte"), &Value::Signed(-29));
    assert_eq!(member(phase, "unknown"), &Value::Signed(-37));
    assert_eq!(canonical, bytes);
}

/// Perk fragment count and index do not borrow quest or event header layouts.
#[test]
fn perk_fragment_has_its_own_count_and_signed_unknowns() {
    let bytes = fixture::field(5, 1, &[], &fixture::perk_tail());
    let (value, canonical) = vmad::decode(&bytes, b"PERK", Ok).unwrap();
    let fragment = element(member(member(&value, "fragments"), "entries"), 0);
    assert_eq!(member(fragment, "index"), &Value::Unsigned(751));
    assert_eq!(member(fragment, "unknown_word"), &Value::Signed(-497));
    assert_eq!(member(fragment, "unknown"), &Value::Signed(-41));
    assert_eq!(canonical, bytes);
}

/// Unknown event bits have known framing and retain their numeric identity without invention.
#[test]
fn fragment_event_bits_with_unknown_meaning_retain_defined_named_layout() {
    for owner in [b"INFO", b"PACK", b"SCEN"] {
        let mut tail = fixture::events(
            0x84,
            &[
                fixture::named_fragment(-3, "UnknownFour", "Four"),
                fixture::named_fragment(-5, "UnknownHigh", "High"),
            ],
        );
        if owner == b"SCEN" {
            tail.extend_from_slice(&0u16.to_le_bytes());
        }
        let bytes = fixture::field(5, 2, &[], &tail);
        let (value, canonical) = vmad::decode(&bytes, owner, Ok).unwrap();
        let events = member(member(&value, "fragments"), "events");
        assert_eq!(member(element(events, 0), "event"), &Value::Unsigned(4));
        assert_eq!(member(element(events, 1), "event"), &Value::Unsigned(128));
        assert_eq!(canonical, bytes);
    }
}

/// The existing script SQL projection can represent an explicit absent property.
#[test]
fn legacy_primary_parser_keeps_none_and_the_following_property() {
    use converter::esm::records::record_type::vmad::{ScriptPropertyValue, parse_vmad};
    let properties = [
        fixture::property(5, "Absent", 0, 3, &[]),
        fixture::property(5, "FollowingInt", 3, 1, &(-7391i32).to_le_bytes()),
    ];
    let bytes = fixture::field(
        5,
        2,
        &[fixture::script(5, "NoneScript", 0, &properties)],
        &[],
    );
    let (remaining, parsed) =
        parse_vmad(&bytes, b"NPC_").expect("native type0 must not drop the script");
    assert!(remaining.is_empty());
    assert_eq!(parsed.scripts[0].properties.len(), 2);
    assert_eq!(parsed.scripts[0].properties[0].property_type, 0);
    assert_eq!(
        parsed.scripts[0].properties[0].value,
        ScriptPropertyValue::None
    );
    assert_eq!(
        parsed.scripts[0].properties[1].value,
        ScriptPropertyValue::Int(-7391)
    );
}

/// Quest aliases use the outer object format, then each alias's separate inner format.
#[test]
fn quest_alias_objects_and_alias_scripts_all_remap_with_separate_headers() {
    for format in [1, 2] {
        let bytes = fixture::field(
            5,
            format,
            &[],
            &fixture::quest_tail(format, 0x02001731, 0x02002957),
        );
        let (value, canonical) = vmad::decode(&bytes, b"QUST", |id| Ok(id ^ 0x07000000)).unwrap();
        let fragments = member(&value, "fragments");
        let fragment = element(member(fragments, "entries"), 0);
        assert_eq!(member(fragment, "stage"), &Value::Unsigned(317));
        assert_eq!(member(fragment, "stage_index"), &Value::Signed(-87123));
        let aliases = member(member(fragments, "aliases"), "entries");
        for (index, expected) in [0x05001731, 0x05002957].into_iter().enumerate() {
            let alias = element(aliases, index);
            assert_eq!(
                member(member(alias, "object"), "form_id"),
                &Value::FormId(expected)
            );
            assert_eq!(
                member(alias, "object_format"),
                &Value::Signed(index as i64 + 1)
            );
            let prop = element(
                member(element(member(alias, "scripts"), 0), "properties"),
                0,
            );
            assert_eq!(
                member(member(prop, "value"), "form_id"),
                &Value::FormId(expected)
            );
            assert_eq!(
                member(member(prop, "value"), "unused"),
                &Value::Unsigned(0xD149)
            );
        }
        let (again, _) = vmad::decode(&canonical, b"QUST", Ok).unwrap();
        assert_eq!(again, value);
    }
}

/// Hostile counts, unsupported layouts and incomplete tails cannot allocate or silently pass.
#[test]
fn malformed_vmad_boundaries_and_counts_are_local_errors() {
    let cases = [
        vec![5, 0, 3, 0, 0, 0],
        vec![99, 0, 2, 0, 0, 0],
        vec![5, 0, 2, 0, 255, 255],
        fixture::field(
            5,
            2,
            &[fixture::script(
                5,
                "BadArray",
                0,
                &[fixture::property(
                    5,
                    "Array",
                    11,
                    1,
                    &u32::MAX.to_le_bytes(),
                )],
            )],
            &[],
        ),
        fixture::field(
            5,
            2,
            &[fixture::script(
                5,
                "InvalidUtf8",
                0,
                &[fixture::property(5, "String", 2, 1, &[1, 0, 255])],
            )],
            &[],
        ),
        fixture::field(
            5,
            2,
            &[fixture::script(
                5,
                "Nonfinite",
                0,
                &[fixture::property(5, "Float", 4, 1, &f32::NAN.to_le_bytes())],
            )],
            &[],
        ),
        fixture::field(
            5,
            2,
            &[fixture::script(
                5,
                "UnknownType",
                0,
                &[fixture::property(5, "Unknown", 8, 1, &[])],
            )],
            &[],
        ),
    ];
    for bytes in cases {
        assert!(vmad::decode(&bytes, b"WEAP", Ok).is_err());
    }
    let full = fixture::field(5, 2, &[], &fixture::quest_tail(2, 0x01001731, 0x01002957));
    for end in 0..full.len() {
        if end != 6 {
            assert!(
                vmad::decode(&full[..end], b"QUST", Ok).is_err(),
                "accepted truncated offset {end}"
            );
        }
    }
    assert!(vmad::decode(&full, b"WEAP", Ok).is_err());
    let missing_flagged_fragment = fixture::field(5, 2, &[], &fixture::events(128, &[]));
    assert!(vmad::decode(&missing_flagged_fragment, b"INFO", Ok).is_err());
    let mut trailing = fixture::field(5, 2, &[], &[]);
    trailing.push(99);
    assert!(vmad::decode(&trailing, b"NPC_", Ok).is_err());
}

/// Independently frame a script-bearing record with a source-preserved editor ID.
fn owner_record(kind: &[u8; 4], id: u32, vmad_bytes: &[u8]) -> Vec<u8> {
    use dummy_content::inhouse_actors as native;
    let payload = [
        native::subrecord(b"EDID", &native::text(&format!("ScriptOwner{id:X}"))),
        native::subrecord(b"VMAD", vmad_bytes),
    ]
    .concat();
    native::record(kind, id, 0, &payload)
}

/// Public reading resolves asymmetric full/light slots in primary objects and every alias.
#[test]
fn public_reader_resolves_primary_and_alias_links_with_divergent_full_and_light_slots() {
    use converter::{esm::load_order::LoadOrder, records::read_plugins};
    use dummy_content::{esm, inhouse_actors as native, layout};
    use std::fs;
    let generated = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    fs::write(
        data.join("Prelude.esm"),
        native::plugin(&generated, &[], 1, &[]),
    )
    .unwrap();
    let target = native::record(
        b"KYWD",
        0x26F0,
        0,
        &native::subrecord(b"EDID", &native::text("FullTarget")),
    );
    fs::write(
        data.join("Objects.esm"),
        native::plugin(&generated, &[], 1, &target),
    )
    .unwrap();
    let light_target = native::record(
        b"KYWD",
        0x010008A3,
        0,
        &native::subrecord(b"EDID", &native::text("LightTarget")),
    );
    fs::write(
        data.join("Light.esl"),
        native::plugin(&generated, &["Skyrim.esm"], 0x201, &light_target),
    )
    .unwrap();
    let props = [
        fixture::property(
            5,
            "FullTarget",
            1,
            1,
            &fixture::object(2, 0x010026F0, -1, 0xCA73),
        ),
        fixture::property(
            5,
            "LightTarget",
            1,
            1,
            &fixture::object(2, 0x000008A3, 31, 0xD539),
        ),
        fixture::property(
            5,
            "MissingTarget",
            1,
            1,
            &fixture::object(2, 0x01002FFF, -1, 0xE751),
        ),
        fixture::property(
            5,
            "BadMaster",
            1,
            1,
            &fixture::object(2, 0x070026F0, -1, 0xF953),
        ),
    ];
    let primary = fixture::script(5, "PublicScript", 1, &props);
    let source = fixture::field(
        5,
        2,
        &[primary],
        &fixture::quest_tail(2, 0x010026F0, 0x000008A3),
    );
    fs::write(
        data.join("Scripts.esp"),
        native::plugin(
            &generated,
            &["Light.esl", "Objects.esm"],
            0,
            &owner_record(b"QUST", 0x02004200, &source),
        ),
    )
    .unwrap();
    let paths: Vec<_> = [
        "Skyrim.esm",
        "Prelude.esm",
        "Objects.esm",
        "Light.esl",
        "Scripts.esp",
    ]
    .into_iter()
    .map(|name| data.join(name))
    .collect();
    let order = LoadOrder::read(&paths).unwrap();
    assert_eq!(order.normal["scripts.esp"], 3);
    let result = read_plugins(&paths, &order).unwrap();
    let record = &result.records[&0x03004200];
    let field = record
        .fields
        .iter()
        .find(|field| field.signature == *b"VMAD")
        .unwrap();
    let properties = member(element(member(&field.value, "scripts"), 0), "properties");
    for (index, expected) in [0x020026F0, 0xFE0008A3, 0, 0].into_iter().enumerate() {
        assert_eq!(
            member(member(element(properties, index), "value"), "form_id"),
            &Value::FormId(expected)
        );
    }
    let aliases = member(
        member(member(&field.value, "fragments"), "aliases"),
        "entries",
    );
    for (index, expected) in [0x020026F0, 0xFE0008A3].into_iter().enumerate() {
        let alias = element(aliases, index);
        assert_eq!(
            member(member(alias, "object"), "form_id"),
            &Value::FormId(expected)
        );
        let property = element(
            member(element(member(alias, "scripts"), 0), "properties"),
            0,
        );
        assert_eq!(
            member(member(property, "value"), "form_id"),
            &Value::FormId(expected)
        );
    }
    assert!(
        record
            .raw_payload
            .windows(source.len())
            .any(|bytes| bytes == source)
    );
    assert_eq!(result.diagnostics["scripts.esp"].invalid_links, 2);
    assert_eq!(result.diagnostics["scripts.esp"].skipped_fields, 0);
}

/// A broken VMAD field is omitted locally while usable neighboring records publish.
#[tokio::test]
async fn malformed_script_fields_publish_neighbors_and_preserve_source_bytes() {
    use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
    use dummy_content::{inhouse_actors as native, layout};
    use std::fs;
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("pack");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let generated = fs::read(data.join("Skyrim.esm")).unwrap();
    let valid = fixture::field(
        5,
        2,
        &[fixture::script(
            5,
            "NoneScript",
            0,
            &[
                fixture::property(5, "Absent", 0, 3, &[]),
                fixture::property(5, "FollowingInt", 3, 1, &(-7391i32).to_le_bytes()),
            ],
        )],
        &fixture::quest_tail(2, 0x14, 0),
    );
    let mut malformed = valid.clone();
    malformed.pop();
    let hostile = fixture::field(
        5,
        2,
        &[fixture::script(
            5,
            "HostileArray",
            0,
            &[fixture::property(
                5,
                "Array",
                11,
                1,
                &u32::MAX.to_le_bytes(),
            )],
        )],
        &[],
    );
    let records = [
        owner_record(b"QUST", 0x01004300, &valid),
        owner_record(b"QUST", 0x01004301, &malformed),
        owner_record(b"QUST", 0x01004302, &hostile),
        owner_record(b"QUST", 0x01004303, &valid),
    ]
    .concat();
    fs::write(
        data.join("Scripts.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0, &records),
    )
    .unwrap();
    fs::write(data.join("plugins.txt"), "*Skyrim.esm\n*Scripts.esp\n").unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in 0x01004300u32..=0x01004303 {
        let blob: Vec<u8> = db
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let canonical = rkyv::from_bytes::<
            converter::esm::types::ArchivedRecordData,
            rkyv::rancor::Error,
        >(&blob)
        .unwrap();
        assert!(
            canonical
                .subrecords
                .iter()
                .any(|field| field.tag == *b"EDID")
        );
        assert_eq!(
            canonical
                .subrecords
                .iter()
                .any(|field| field.tag == *b"VMAD"),
            matches!(id, 0x01004300 | 0x01004303)
        );
    }
    for id in [0x01004300u32, 0x01004303] {
        let properties: String = db
            .query_row(
                "SELECT properties_json FROM scripts WHERE form_id=?1 AND script_name='NoneScript'",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        let properties: serde_json::Value = serde_json::from_str(&properties).unwrap();
        assert_eq!(properties[0]["property_type"], 0);
        assert_eq!(properties[0]["value"], "None");
        assert_eq!(properties[1]["value"]["Int"], -7391);
    }
    let source: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [0x01004301u32],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        source
            .windows(malformed.len())
            .any(|bytes| bytes == malformed)
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["scripts.esp"]["skipped_fields"], 2);
    assert_eq!(diagnostics["decoder"]["scripts.esp"]["skipped_records"], 0);
    assert!(
        diagnostics["decoder"]["scripts.esp"]["first_by_category"]["field"]
            .as_str()
            .unwrap()
            .contains("VMAD")
    );
    assert!(output.join("cell_cache.rkyv").is_file());
    assert!(output.join("conversion-manifest.json").is_file());
}
