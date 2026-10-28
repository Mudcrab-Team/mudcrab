//! Dummy fixtures for the reference mapping of the cluster store (`shared::cluster_mapping`): a
//! tiny package with real `.mci` bytes from the writer, and one case per way the mapping can be
//! wrong. The data is invented; no converted dataset is read.

use converter::cache::hash_bytes;
use rusqlite::Connection;
use shared::{
    cluster::{
        CLUSTER_TABLES_DDL, InstanceInput, MciInputs, MeshRef, hex, instance_payload_path,
        write_mci, write_mci_ordered,
    },
    cluster_mapping::{
        CLUSTER_MAPPING_DDL, COVERAGE_VERSION, CoverageState, DIGEST_VERSION, Eligibility,
        FixedAuthority, INSTANCE_MAP_VERSION, MappingError, MaterialSource, ReferenceInputs,
        ReferenceSnapshot, coverage_state, input_digest, validate_mapping,
        validate_mapping_structure,
    },
};

const BUILDER: [u8; 32] = [0xab; 32];
const BLOCK_WORLD: u32 = 0x3c;
const CHILD_WORLD: u32 = 0x1_a22f;
const SOURCE_CELL: u32 = 7;
const IDENTITY: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn built_from() -> String {
    "cd".repeat(32)
}

struct Placed {
    reference: u32,
    model: u8,
    primitive: u32,
    cell: (i32, i32),
    base: u32,
    child: bool,
}

fn model_hex(model: u8) -> String {
    hex(&[model; 32])
}

fn placed() -> Vec<Placed> {
    let at = |reference, model, primitive, cell, base, child| Placed {
        reference,
        model,
        primitive,
        cell,
        base,
        child,
    };
    vec![
        at(0x100, 1, 0, (0, 0), 0x11, false),
        at(0x200, 2, 0, (0, 0), 0x22, false),
        at(0x200, 2, 1, (0, 0), 0x22, false),
        at(0x300, 3, 0, (1, 0), 0x33, false),
        at(0x600, 1, 0, (1, 0), 0x11, true),
        // Two references that sort equal: the writer's order keeps their input order.
        at(0x700, 1, 0, (1, 0), 0x11, false),
        at(0x800, 1, 0, (1, 0), 0x11, false),
    ]
}

fn world_of(reference: u32) -> u32 {
    if placed().iter().any(|p| p.reference == reference && p.child) {
        CHILD_WORLD
    } else {
        BLOCK_WORLD
    }
}

fn base_of(reference: u32) -> u32 {
    placed()
        .iter()
        .find(|p| p.reference == reference)
        .map_or(0, |p| p.base)
}

fn inputs_of(reference: u32) -> ReferenceInputs<'static> {
    ReferenceInputs {
        reference_id: reference,
        base_form_id: base_of(reference),
        record_type: "STAT",
        placed_worldspace_id: world_of(reference),
        source_cell_id: SOURCE_CELL,
        model_path: Some("meshes/x.glb"),
        model_hash: Some("00"),
        position: [1.0, 2.0, 3.0],
        rotation: [0.0; 3],
        scale: 1.0,
        header_flags: 0,
        enable_parent_id: None,
        snow_material: 0,
        data_hash: Some("00"),
        base_record_hash: Some("11"),
        texture_set_hash: Some("22"),
    }
}

fn digest_of(reference: u32) -> String {
    input_digest(&inputs_of(reference))
}

struct Fixture {
    directory: tempfile::TempDir,
    connection: Connection,
}

fn fixture() -> Fixture {
    fixture_from(placed())
}

/// The fixture for a given set of placements: the mesh rows, the block, the map and the coverage
/// all follow it, so removing a placement removes it everywhere the stage would.
fn fixture_from(placed: Vec<Placed>) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
    connection.execute_batch(CLUSTER_TABLES_DDL).unwrap();
    connection.execute_batch(CLUSTER_MAPPING_DDL).unwrap();
    connection
        .execute(
            "INSERT INTO cluster_build(id, builder_identity, triangle_threshold, block_side_cells, lod_mode)
             VALUES (1, ?1, 0, 16, 'cluster')",
            [hex(&BUILDER)],
        )
        .unwrap();
    let built: std::collections::BTreeSet<(u8, u32)> =
        placed.iter().map(|p| (p.model, p.primitive)).collect();
    for (model, primitive) in built {
        connection
            .execute(
                "INSERT INTO cluster_meshes(mesh_hash, primitive_index, payload_path, content_hash,
                     source_triangles, cluster_count, vertex_count, payload_bytes, material_key)
                 VALUES (?1, ?2, 'x', 'x', 1, 1, 1, 1, 'm')",
                rusqlite::params![model_hex(model), primitive],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO cluster_rejects(mesh_hash, primitive_index, reason) VALUES (?1, 1, 'skinned')",
            [model_hex(3)],
        )
        .unwrap();

    let inputs: Vec<InstanceInput> = placed
        .iter()
        .map(|p| InstanceInput {
            mesh: MeshRef {
                mesh_hash: [p.model; 32],
                primitive_index: p.primitive,
            },
            transform: IDENTITY,
            cell_x: p.cell.0,
            cell_y: p.cell.1,
            stable_id: [0; 16],
            base_form_id: p.base,
            snow_material: 0,
            source_worldspace_id: if p.child { CHILD_WORLD } else { 0 },
            world_min: [0.0; 3],
            world_max: [1.0; 3],
        })
        .collect();
    let (bytes, order) = write_mci_ordered(&MciInputs {
        builder_identity: BUILDER,
        worldspace_id: BLOCK_WORLD,
        tier: 16,
        anchor_x: 0,
        anchor_y: 0,
        instances: &inputs,
    })
    .unwrap();
    let path = instance_payload_path(BLOCK_WORLD, 16, 0, 0);
    std::fs::create_dir_all(directory.path().join(&path).parent().unwrap()).unwrap();
    std::fs::write(directory.path().join(&path), &bytes).unwrap();
    let chunk_hash = hash_bytes(&bytes);
    connection
        .execute(
            "INSERT INTO cluster_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash,
                 input_key, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y,
                 bounds_max_z, instance_count, source_cells)
             VALUES (?1, 16, 0, 0, ?2, ?3, 'k', 0, 0, 0, 1, 1, 1, ?4, '')",
            rusqlite::params![BLOCK_WORLD, path, chunk_hash, inputs.len()],
        )
        .unwrap();

    connection
        .execute(
            "INSERT INTO cluster_provenance(id, instance_map_version, coverage_version, digest_version,
                 digest_algorithm, built_from, builder_identity, classifier_scope)
             VALUES (1, ?1, ?2, ?3, 'sha256', ?4, ?5, '{\"fixture\":true}')",
            rusqlite::params![
                INSTANCE_MAP_VERSION,
                COVERAGE_VERSION,
                DIGEST_VERSION,
                built_from(),
                hex(&BUILDER)
            ],
        )
        .unwrap();
    for (ordinal, &input_index) in order.iter().enumerate() {
        let p = &placed[input_index];
        connection
            .execute(
                "INSERT INTO cluster_instance_map(worldspace_id, tier, anchor_x, anchor_y,
                     chunk_content_hash, ordinal, reference_id, placed_worldspace_id, cell_x, cell_y,
                     model_hash, primitive_index, input_digest)
                 VALUES (?1, 16, 0, 0, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    BLOCK_WORLD,
                    chunk_hash,
                    ordinal,
                    p.reference,
                    world_of(p.reference),
                    p.cell.0,
                    p.cell.1,
                    model_hex(p.model),
                    p.primitive,
                    digest_of(p.reference)
                ],
            )
            .unwrap();
    }
    let cover = |reference: u32,
                 scope: &str,
                 model: &str,
                 primitive: i64,
                 status: &str,
                 reason: &str| {
        connection
                .execute(
                    "INSERT INTO cluster_reference_coverage(reference_id, placed_worldspace_id,
                         source_cell_id, scope, model_hash, primitive_index, status, reason, input_digest)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        reference,
                        world_of(reference),
                        SOURCE_CELL,
                        scope,
                        model,
                        primitive,
                        status,
                        reason,
                        digest_of(reference)
                    ],
                )
                .unwrap();
    };
    let mut expanded = std::collections::BTreeSet::new();
    for p in &placed {
        if expanded.insert(p.reference) {
            cover(
                p.reference,
                "reference",
                &model_hex(p.model),
                -1,
                "expanded",
                "",
            );
        }
        cover(
            p.reference,
            "primitive",
            &model_hex(p.model),
            i64::from(p.primitive),
            "clustered",
            "",
        );
    }
    // The model of 0x300 has a second primitive the builder rejected.
    cover(0x300, "primitive", &model_hex(3), 1, "rejected", "skinned");
    // Refused as wholes: a classifier rule, and an override the converter cannot resolve.
    cover(0x400, "reference", "", -1, "not_eligible", "scripted");
    cover(
        0x500,
        "reference",
        "",
        -1,
        "not_eligible",
        "unresolved_reference_override:XALP",
    );
    // A reference whose model could not be resolved: the expected set is unknown.
    cover(0x900, "reference", "", -1, "unknown", "model_not_loaded");
    Fixture {
        directory,
        connection,
    }
}

/// The authority's snapshot of one reference: every typed field the package must agree with.
fn snapshot_of(
    reference: u32,
    cell: (i32, i32),
    model: Option<String>,
    eligibility: Eligibility,
) -> ReferenceSnapshot {
    ReferenceSnapshot {
        base_form_id: base_of(reference),
        snow_material: 0,
        placed_worldspace_id: world_of(reference),
        source_cell_id: SOURCE_CELL,
        cell,
        block: (BLOCK_WORLD, 16, 0, 0),
        digest: digest_of(reference),
        transform: model.as_ref().map(|_| IDENTITY),
        model,
        eligibility,
    }
}

/// What the converter's own inputs say about the fixture.
fn truth() -> FixedAuthority {
    let mut authority = FixedAuthority {
        core_build_identity: Some(built_from()),
        builder_identity: hex(&BUILDER),
        classifier_scope: "{\"fixture\":true}".to_owned(),
        ..FixedAuthority::default()
    };
    for p in placed() {
        authority.references.entry(p.reference).or_insert_with(|| {
            snapshot_of(
                p.reference,
                p.cell,
                Some(model_hex(p.model)),
                Eligibility::Eligible,
            )
        });
    }
    // The three refused as wholes: a classifier rule, an override, an unresolved model.
    authority.references.insert(
        0x400,
        snapshot_of(
            0x400,
            (0, 0),
            None,
            Eligibility::NotEligible("scripted".to_owned()),
        ),
    );
    authority.references.insert(
        0x500,
        snapshot_of(
            0x500,
            (0, 0),
            None,
            Eligibility::NotEligible("unresolved_reference_override:XALP".to_owned()),
        ),
    );
    authority.references.insert(
        0x900,
        snapshot_of(
            0x900,
            (0, 0),
            None,
            Eligibility::Unknown("model_not_loaded".to_owned()),
        ),
    );
    for (model, primitives) in [(1u8, vec![0u32]), (2, vec![0, 1]), (3, vec![0, 1])] {
        authority.materials.insert(
            model_hex(model),
            primitives
                .iter()
                .map(|&p| {
                    (
                        p,
                        MaterialSource {
                            model_path: String::new(),
                            material_index: None,
                            material_key: "m".to_owned(),
                            alpha_mask: false,
                            alpha_cutoff: None,
                            double_sided: false,
                        },
                    )
                })
                .collect(),
        );
        authority
            .models
            .insert(model_hex(model), primitives.into_iter().collect());
    }
    authority
}

fn check_with(fixture: &Fixture, authority: &FixedAuthority) -> Result<(), MappingError> {
    validate_mapping(
        &fixture.connection,
        fixture.directory.path(),
        Some(&built_from()),
        authority,
    )
}

fn check(fixture: &Fixture) -> Result<(), MappingError> {
    check_with(fixture, &truth())
}

fn check_structure(fixture: &Fixture) -> Result<(), MappingError> {
    validate_mapping_structure(
        &fixture.connection,
        fixture.directory.path(),
        Some(&built_from()),
    )
}

fn run(fixture: &Fixture, sql: &str) {
    fixture.connection.execute_batch(sql).unwrap();
}

#[test]
fn a_consistent_package_validates_and_states_each_reference() {
    let fixture = fixture();
    check(&fixture).unwrap();
    let state = |reference| coverage_state(&fixture.connection, reference).unwrap();
    assert_eq!(state(0x100), CoverageState::Complete);
    assert_eq!(state(0x200), CoverageState::Complete);
    assert_eq!(state(0x300), CoverageState::Partial);
    assert_eq!(state(0x400), CoverageState::NotEligible);
    assert_eq!(state(0x500), CoverageState::NotEligible);
    assert_eq!(state(0x900), CoverageState::Unknown);
    assert_eq!(state(0x999), CoverageState::Absent);
}

#[test]
fn equal_sort_keys_keep_input_order() {
    let fixture = fixture();
    let ordinal_of = |reference: u32| -> i64 {
        fixture
            .connection
            .query_row(
                "SELECT ordinal FROM cluster_instance_map WHERE reference_id = ?1",
                [reference],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(ordinal_of(0x800), ordinal_of(0x700) + 1);
}

#[test]
fn a_missing_ordinal_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        "DELETE FROM cluster_instance_map WHERE ordinal = 2",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::OrdinalGap { .. })
    ));
}

#[test]
fn a_duplicate_ordinal_cannot_be_stored() {
    let fixture = fixture();
    let duplicate = fixture.connection.execute(
        "INSERT INTO cluster_instance_map SELECT * FROM cluster_instance_map WHERE ordinal = 0",
        [],
    );
    assert!(duplicate.is_err());
}

#[test]
fn a_placement_cannot_be_mapped_twice() {
    // One reference contributes one record per primitive: a second record for the same
    // (reference, model, primitive) is refused by the unique index.
    let fixture = fixture();
    let again = fixture.connection.execute(
        "INSERT INTO cluster_instance_map SELECT worldspace_id, tier, anchor_x, anchor_y,
             chunk_content_hash, 99, reference_id, placed_worldspace_id, cell_x, cell_y, model_hash,
             primitive_index, input_digest FROM cluster_instance_map WHERE ordinal = 0",
        [],
    );
    assert!(again.is_err());
}

#[test]
fn an_ordinal_past_the_last_record_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        "INSERT INTO cluster_instance_map SELECT worldspace_id, tier, anchor_x, anchor_y,
             chunk_content_hash, 99, 99999, placed_worldspace_id, cell_x, cell_y, model_hash,
             primitive_index, input_digest FROM cluster_instance_map WHERE ordinal = 0",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::OrdinalGap { .. })
    ));
}

#[test]
fn rows_swapped_between_ordinals_do_not_match_the_records() {
    let fixture = fixture();
    run(
        &fixture,
        "UPDATE cluster_instance_map SET ordinal = 1000 WHERE ordinal = 0;
         UPDATE cluster_instance_map SET ordinal = 0 WHERE ordinal = 3;
         UPDATE cluster_instance_map SET ordinal = 3 WHERE ordinal = 1000;",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::RecordMismatch { .. })
    ));
}

#[test]
fn a_wrong_mesh_or_primitive_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        "UPDATE cluster_instance_map SET primitive_index = 1 WHERE ordinal = 0",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::RecordMismatch { what: "mesh", .. })
    ));
}

#[test]
fn a_mesh_the_store_does_not_hold_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        &format!(
            "DELETE FROM cluster_meshes WHERE primitive_index = 0 AND mesh_hash = '{}'",
            model_hex(3)
        ),
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::UnknownMesh { .. })
    ));
}

#[test]
fn stale_provenance_is_refused() {
    let fixture = fixture();
    assert_eq!(
        validate_mapping_structure(
            &fixture.connection,
            fixture.directory.path(),
            Some(&"ee".repeat(32))
        ),
        Err(MappingError::StaleBuiltFrom)
    );
    run(
        &fixture,
        &format!(
            "UPDATE cluster_provenance SET builder_identity = '{}'",
            "11".repeat(32)
        ),
    );
    assert_eq!(check(&fixture), Err(MappingError::StaleBuilderIdentity));
    run(&fixture, "DELETE FROM cluster_provenance");
    assert_eq!(check(&fixture), Err(MappingError::NoProvenance));
}

#[test]
fn versions_and_identities_are_checked_by_the_tables() {
    let fixture = fixture();
    let bad = |sql: &str| fixture.connection.execute(sql, []).is_err();
    assert!(bad(
        "UPDATE cluster_provenance SET instance_map_version = 2"
    ));
    assert!(bad("UPDATE cluster_provenance SET coverage_version = 2"));
    assert!(bad("UPDATE cluster_provenance SET digest_version = 2"));
    assert!(bad(
        "UPDATE cluster_provenance SET digest_algorithm = 'md5'"
    ));
    // 64 characters is not enough: it must be lower-case hex.
    assert!(bad(&format!(
        "UPDATE cluster_provenance SET built_from = '{}'",
        "zz".repeat(32)
    )));
    assert!(bad(&format!(
        "UPDATE cluster_provenance SET builder_identity = '{}'",
        "AB".repeat(32)
    )));
    assert!(bad("UPDATE cluster_provenance SET classifier_scope = ''"));
    assert!(bad(&format!(
        "UPDATE cluster_instance_map SET input_digest = '{}'",
        "g".repeat(64)
    )));
}

#[test]
fn a_stale_input_digest_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        &format!(
            "UPDATE cluster_reference_coverage SET input_digest = '{}' WHERE reference_id = 256",
            "22".repeat(32)
        ),
    );
    assert_eq!(check(&fixture), Err(MappingError::DigestMismatch(0x100)));
}

#[test]
fn a_changed_chunk_file_is_refused() {
    let fixture = fixture();
    let path = fixture
        .directory
        .path()
        .join(instance_payload_path(BLOCK_WORLD, 16, 0, 0));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        check(&fixture),
        Err(MappingError::ChunkHashMismatch(_))
    ));
}

#[test]
fn a_map_row_bound_to_another_content_hash_is_refused() {
    let fixture = fixture();
    run(
        &fixture,
        &format!(
            "UPDATE cluster_instance_map SET chunk_content_hash = '{}' WHERE ordinal = 0",
            "77".repeat(32)
        ),
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::ChunkHashMismatch(_))
    ));
}

#[test]
fn map_rows_of_an_unlisted_block_are_refused() {
    let fixture = fixture();
    run(
        &fixture,
        "INSERT INTO cluster_instance_map SELECT worldspace_id, tier, 160, anchor_y,
             chunk_content_hash, ordinal, reference_id + 100000, placed_worldspace_id, cell_x, cell_y,
             model_hash, primitive_index, input_digest
         FROM cluster_instance_map WHERE ordinal = 0",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::UnknownChunk(_))
    ));
}

#[test]
fn identical_bytes_under_another_anchor_are_not_aliased() {
    // The same file listed for a second logical block: its header names the first one, so the
    // second row cannot claim it, and the map rows are keyed by the logical block, not by hash.
    let fixture = fixture();
    run(
        &fixture,
        "INSERT INTO cluster_chunks SELECT worldspace_id, tier, 16, anchor_y, payload_path,
             content_hash, input_key, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x,
             bounds_max_y, bounds_max_z, instance_count, source_cells FROM cluster_chunks",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::ChunkHeaderMismatch(_))
    ));
}

#[test]
fn a_chunk_header_must_agree_with_its_row_and_the_provenance() {
    let fixture = fixture();
    for (set, restore) in [
        ("anchor_x = 16", "anchor_x = 0"),
        ("tier = 32", "tier = 16"),
        ("worldspace_id = 61", "worldspace_id = 60"),
        ("instance_count = 9", "instance_count = 7"),
    ] {
        run(&fixture, &format!("UPDATE cluster_chunks SET {set}"));
        assert!(
            matches!(check(&fixture), Err(MappingError::ChunkHeaderMismatch(_))),
            "{set}"
        );
        run(&fixture, &format!("UPDATE cluster_chunks SET {restore}"));
    }
    // A block built by another builder than the one the provenance names.
    let other = "11".repeat(32);
    run(
        &fixture,
        &format!(
            "UPDATE cluster_build SET builder_identity = '{other}';
             UPDATE cluster_provenance SET builder_identity = '{other}'"
        ),
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::ChunkHeaderMismatch(_))
    ));
}

#[test]
fn terrain_instances_are_not_object_blocks() {
    let fixture = fixture();
    run(
        &fixture,
        "INSERT INTO cluster_chunks SELECT worldspace_id, tier, 32, anchor_y,
             'clusters/terrain/instances/ab/x.mci', content_hash, input_key, bounds_min_x,
             bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z, instance_count,
             source_cells FROM cluster_chunks",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::TerrainInObjectChunks(_))
    ));
}

#[test]
fn a_model_without_geometry_expands_to_no_primitives() {
    // Mods hide a vanilla object by giving its base an empty NIF (a root node, no shapes): the
    // reference resolves to a model with no primitives, so it has no mesh, reject or map rows.
    let fixture = fixture();
    let empty = model_hex(10);
    run(
        &fixture,
        &format!(
            "INSERT INTO cluster_reference_coverage(reference_id, placed_worldspace_id,
                 source_cell_id, scope, model_hash, primitive_index, status, reason, input_digest)
             VALUES ({reference}, {world}, {cell}, 'reference', '{empty}', -1, 'expanded', '',
                 '{digest}')",
            reference = 0xa00,
            world = world_of(0xa00),
            cell = SOURCE_CELL,
            digest = digest_of(0xa00),
        ),
    );
    let mut authority = truth();
    authority.references.insert(
        0xa00,
        snapshot_of(0xa00, (0, 0), Some(empty.clone()), Eligibility::Eligible),
    );
    authority
        .materials
        .insert(empty.clone(), Default::default());
    authority.models.insert(empty, Default::default());
    check_with(&fixture, &authority).unwrap();
}

#[test]
fn coverage_and_instances_must_agree() {
    // No state row at all for a mapped reference.
    let fixture = fixture();
    run(
        &fixture,
        "DELETE FROM cluster_reference_coverage WHERE reference_id = 256",
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::ReferenceStateMissing(0x100))
    );

    // A state row but no clustered row for the instance.
    let fixture = self::fixture();
    run(
        &fixture,
        "DELETE FROM cluster_reference_coverage WHERE reference_id = 256 AND scope = 'primitive'",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::IncompleteExpectedSet(0x100) | MappingError::CoverageMissing(0x100))
    ));

    // A clustered row for a primitive nobody placed.
    let fixture = self::fixture();
    run(
        &fixture,
        &format!(
            "INSERT INTO cluster_reference_coverage
                 SELECT 512, placed_worldspace_id, source_cell_id, 'primitive', '{}', 5, 'clustered', '',
                     input_digest FROM cluster_reference_coverage
                 WHERE reference_id = 512 AND scope = 'reference'",
            model_hex(2)
        ),
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::CoverageWithoutInstance(0x200))
    );
}

#[test]
fn a_rejected_row_must_match_the_reject_table() {
    let fixture = fixture();
    run(
        &fixture,
        "UPDATE cluster_reference_coverage SET reason = 'not_opaque' WHERE status = 'rejected'",
    );
    assert!(matches!(
        check(&fixture),
        Err(MappingError::RejectMismatch { .. })
    ));
}

#[test]
fn a_refused_or_unknown_reference_has_no_primitives_and_no_instances() {
    let fixture = fixture();
    run(
        &fixture,
        "UPDATE cluster_reference_coverage SET status = 'not_eligible', reason = 'scripted'
         WHERE reference_id = 256 AND scope = 'reference'",
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::PrimitiveRowsWithoutExpansion(0x100))
    );
    run(
        &fixture,
        "DELETE FROM cluster_reference_coverage WHERE reference_id = 256 AND scope = 'primitive'",
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::ContradictoryCoverage(0x100))
    );
}

#[test]
fn a_reference_has_exactly_one_state() {
    let fixture = fixture();
    let same_key = fixture.connection.execute(
        "INSERT INTO cluster_reference_coverage
             SELECT reference_id, placed_worldspace_id, source_cell_id, 'reference', model_hash, -1,
                 'not_eligible', 'scripted', input_digest
             FROM cluster_reference_coverage WHERE reference_id = 256 AND scope = 'reference'",
        [],
    );
    assert!(same_key.is_err());
    // Another model would be another primary key, and the state index still allows one per
    // reference.
    let other_model = fixture.connection.execute(
        "INSERT INTO cluster_reference_coverage
             SELECT reference_id, placed_worldspace_id, source_cell_id, 'reference', ?1, -1,
                 'not_eligible', 'scripted', input_digest
             FROM cluster_reference_coverage WHERE reference_id = 256 AND scope = 'reference'",
        [model_hex(9)],
    );
    assert!(other_model.is_err());
}

#[test]
fn digests_of_other_inputs_are_not_authoritative() {
    // Every row of reference 0x100 agrees with every other, but was built from another base form
    // id than the one the core database holds: only the authority can tell.
    let fixture = fixture();
    let wrong = input_digest(&ReferenceInputs {
        base_form_id: 0x10,
        ..inputs_of(0x100)
    });
    for table in ["cluster_instance_map", "cluster_reference_coverage"] {
        run(
            &fixture,
            &format!("UPDATE {table} SET input_digest = '{wrong}' WHERE reference_id = 256"),
        );
    }
    check_structure(&fixture).unwrap();
    assert_eq!(
        check(&fixture),
        Err(MappingError::DigestNotAuthoritative(0x100))
    );
}

#[test]
fn a_primitive_dropped_from_every_output_table_is_caught_by_the_authority() {
    // The rejected primitive of model 3 is removed from the reject table and from the coverage
    // of reference 0x300. The emitted tables are now consistent and the reference looks complete.
    let fixture = fixture();
    run(
        &fixture,
        "DELETE FROM cluster_rejects WHERE primitive_index = 1;
         DELETE FROM cluster_reference_coverage WHERE reference_id = 768 AND status = 'rejected'",
    );
    check_structure(&fixture).unwrap();
    assert_eq!(
        coverage_state(&fixture.connection, 0x300).unwrap(),
        CoverageState::Complete
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::ExpectedPrimitivesMismatch {
            reference_id: 0x300,
            mesh_hash: model_hex(3)
        })
    );
}

#[test]
fn the_resolved_model_and_the_transform_come_from_the_authority() {
    let fixture = fixture();
    let mut other_model = truth();
    other_model.references.get_mut(&0x100).unwrap().model = Some(model_hex(2));
    assert_eq!(
        check_with(&fixture, &other_model),
        Err(MappingError::ModelMismatch(0x100))
    );
    let mut other_transform = truth();
    other_transform
        .references
        .get_mut(&0x300)
        .unwrap()
        .transform = Some({
        let mut moved = IDENTITY;
        moved[9] = 5.0;
        moved
    });
    assert_eq!(
        check_with(&fixture, &other_transform),
        Err(MappingError::TransformMismatch(0x300))
    );
    // A reference the package calls unknown, which the converter could in fact resolve.
    let mut resolvable = truth();
    let snapshot = resolvable.references.get_mut(&0x900).unwrap();
    snapshot.model = Some(model_hex(1));
    snapshot.eligibility = Eligibility::Eligible;
    assert_eq!(
        check_with(&fixture, &resolvable),
        Err(MappingError::EligibilityMismatch(0x900))
    );
}

#[test]
fn typed_fields_are_compared_not_only_the_digest() {
    // The authority's digest stays the one the package carries, so a digest-only check would pass
    // every case below; the typed fields of the snapshot are what the stored rows must match.
    let fixture = fixture();
    for (what, change) in [
        (
            "base_form_id",
            (|s: &mut ReferenceSnapshot| s.base_form_id = 0x99) as fn(&mut ReferenceSnapshot),
        ),
        ("snow_material", |s| s.snow_material = 0x77),
        ("cell", |s| s.cell = (5, 5)),
        ("placed_worldspace", |s| s.placed_worldspace_id = 0x55),
        ("source_cell", |s| s.source_cell_id = SOURCE_CELL + 1),
    ] {
        let mut other = truth();
        change(other.references.get_mut(&0x100).unwrap());
        assert_eq!(
            check_with(&fixture, &other),
            Err(MappingError::SnapshotMismatch {
                reference_id: 0x100,
                what
            }),
            "{what}"
        );
    }
}

#[test]
fn a_synchronized_wrong_cell_or_world_is_caught_by_the_snapshot() {
    // The coverage of reference 0x100 is rewritten to another owning cell in all its rows, so the
    // tables still agree with each other.
    let fixture = fixture();
    run(
        &fixture,
        "UPDATE cluster_reference_coverage SET source_cell_id = 9 WHERE reference_id = 256",
    );
    check_structure(&fixture).unwrap();
    assert_eq!(
        check(&fixture),
        Err(MappingError::SnapshotMismatch {
            reference_id: 0x100,
            what: "source_cell"
        })
    );
}

#[test]
fn the_verdict_decides_the_state_and_complete_needs_eligibility() {
    let fixture = fixture();
    // Every refused or unresolved verdict on a reference the package calls expanded refuses, so a
    // reference with an unresolved override or unparsed data can never reach Complete.
    for verdict in [
        Eligibility::NotEligible("unresolved_reference_override:XALP".to_owned()),
        Eligibility::NotEligible("unresolved_base_alternate_textures".to_owned()),
        Eligibility::Unknown("unparsed_reference_data".to_owned()),
        Eligibility::Unknown("model_not_loaded".to_owned()),
    ] {
        let mut other = truth();
        other.references.get_mut(&0x200).unwrap().eligibility = verdict.clone();
        assert_eq!(
            check_with(&fixture, &other),
            Err(MappingError::EligibilityMismatch(0x200)),
            "{verdict:?}"
        );
    }
    // The reason has to match exactly, and a refusal cannot be reported as an unknown or the
    // other way round.
    let mut other_reason = truth();
    other_reason.references.get_mut(&0x400).unwrap().eligibility =
        Eligibility::NotEligible("scripted_other".to_owned());
    assert_eq!(
        check_with(&fixture, &other_reason),
        Err(MappingError::EligibilityMismatch(0x400))
    );
    let mut other_kind = truth();
    other_kind.references.get_mut(&0x500).unwrap().eligibility =
        Eligibility::Unknown("unresolved_reference_override:XALP".to_owned());
    assert_eq!(
        check_with(&fixture, &other_kind),
        Err(MappingError::EligibilityMismatch(0x500))
    );
    // A reference the authority says is eligible but the package refused, hiding a drawable one.
    let mut eligible = truth();
    eligible.references.get_mut(&0x400).unwrap().eligibility = Eligibility::Eligible;
    assert_eq!(
        check_with(&fixture, &eligible),
        Err(MappingError::EligibilityMismatch(0x400))
    );
}

#[test]
fn a_reference_the_dataset_does_not_have_is_refused() {
    let fixture = fixture();
    let mut authority = truth();
    authority.references.remove(&0x400);
    assert_eq!(
        check_with(&fixture, &authority),
        Err(MappingError::ScopeMismatch {
            reference_id: 0x400,
            in_scope: false
        })
    );
}

#[test]
fn references_asked_about_and_not_asked_about_are_told_apart() {
    let fixture = fixture();
    // The authority examined a reference that has no coverage row: it was asked about and is
    // silently absent.
    let mut authority = truth();
    authority.references.insert(
        0x999,
        snapshot_of(0x999, (0, 0), None, Eligibility::Unknown("x".to_owned())),
    );
    assert_eq!(
        check_with(&fixture, &authority),
        Err(MappingError::ScopeMismatch {
            reference_id: 0x999,
            in_scope: true
        })
    );
    // The coverage has a row for a reference nobody examined.
    run(
        &fixture,
        &format!(
            "INSERT INTO cluster_reference_coverage
                 VALUES (2560, 60, 0, 'reference', '', -1, 'not_eligible', 'scripted', '{}')",
            "66".repeat(32)
        ),
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::ScopeMismatch {
            reference_id: 2560,
            in_scope: false
        })
    );
}

#[test]
fn synthetic_instances_have_no_reference_id() {
    // Terrain blocks are not placed references: reference id 0 is not storable, in either table.
    let fixture = fixture();
    assert!(
        fixture
            .connection
            .execute(
                "INSERT INTO cluster_reference_coverage
                     VALUES (0, 60, 0, 'reference', '', -1, 'not_eligible', 'terrain', ?1)",
                ["33".repeat(32)],
            )
            .is_err()
    );
    assert!(
        fixture
            .connection
            .execute(
                "INSERT INTO cluster_instance_map SELECT worldspace_id, tier, anchor_x, anchor_y,
                     chunk_content_hash, 50, 0, placed_worldspace_id, cell_x, cell_y, model_hash,
                     primitive_index, input_digest FROM cluster_instance_map WHERE ordinal = 0",
                [],
            )
            .is_err()
    );
}

#[test]
fn coverage_rows_follow_their_scope_and_status() {
    let fixture = fixture();
    let bad = |sql: &str| fixture.connection.execute(sql, []).is_err();
    let digest = "44".repeat(32);
    let model = "ab".repeat(32);
    let row = |reference: u32,
               scope: &str,
               model: &str,
               primitive: i32,
               status: &str,
               reason: &str| {
        format!(
            "INSERT INTO cluster_reference_coverage VALUES ({reference}, 60, 0, '{scope}', '{model}', {primitive}, '{status}', '{reason}', '{digest}')"
        )
    };
    // Primitive rows: a model, an index, and clustered or rejected only.
    assert!(bad(&row(1, "primitive", &model, 0, "clustered", "why")));
    assert!(bad(&row(1, "primitive", &model, 0, "rejected", "")));
    assert!(bad(&row(
        1,
        "primitive",
        &model,
        0,
        "not_eligible",
        "scripted"
    )));
    assert!(bad(&row(1, "primitive", &model, 0, "expanded", "")));
    assert!(bad(&row(1, "primitive", "", 0, "rejected", "skinned")));
    assert!(bad(&row(1, "primitive", &model, -1, "rejected", "skinned")));
    // Reference rows: no index, a state, a reason unless expanded, a model exactly when expanded.
    assert!(bad(&row(
        1,
        "reference",
        "",
        0,
        "unknown",
        "model_not_loaded"
    )));
    assert!(bad(&row(1, "reference", "", -1, "clustered", "")));
    assert!(bad(&row(1, "reference", "", -1, "rejected", "skinned")));
    assert!(bad(&row(1, "reference", "", -1, "unknown", "")));
    assert!(bad(&row(
        1,
        "reference",
        &model,
        -1,
        "unknown",
        "model_not_loaded"
    )));
    assert!(bad(&row(1, "reference", "", -1, "expanded", "")));
    assert!(bad(&row(1, "reference", &model, -1, "expanded", "why")));
    // The legal shapes are accepted.
    assert!(!bad(&row(1, "reference", &model, -1, "expanded", "")));
    assert!(!bad(&row(1, "primitive", &model, 0, "clustered", "")));
    assert!(!bad(&row(1, "primitive", &model, 1, "rejected", "skinned")));
    assert!(!bad(&row(
        2,
        "reference",
        "",
        -1,
        "unknown",
        "model_not_loaded"
    )));
    assert!(!bad(&row(
        3,
        "reference",
        "",
        -1,
        "not_eligible",
        "scripted"
    )));
    // Hashes are lower-case hex.
    assert!(bad(&row(
        1,
        "primitive",
        &"AB".repeat(32),
        2,
        "clustered",
        ""
    )));
    assert!(bad(&row(1, "primitive", "abc", 2, "clustered", "")));
}

#[test]
fn the_input_digest_changes_with_every_input() {
    let base = ReferenceInputs {
        reference_id: 1,
        base_form_id: 2,
        record_type: "STAT",
        placed_worldspace_id: 60,
        source_cell_id: 4,
        model_path: Some("meshes/a.glb"),
        model_hash: Some("ab"),
        position: [1.0, 2.0, 3.0],
        rotation: [0.0, 0.5, 0.0],
        scale: 1.0,
        header_flags: 0,
        enable_parent_id: None,
        snow_material: 0,
        data_hash: Some("aa"),
        base_record_hash: Some("bb"),
        texture_set_hash: Some("cc"),
    };
    let reference = input_digest(&base);
    assert_eq!(reference, input_digest(&base));
    assert_eq!(reference.len(), 64);
    let variants = [
        ReferenceInputs {
            reference_id: 3,
            ..base
        },
        ReferenceInputs {
            base_form_id: 3,
            ..base
        },
        ReferenceInputs {
            record_type: "TREE",
            ..base
        },
        ReferenceInputs {
            placed_worldspace_id: 61,
            ..base
        },
        ReferenceInputs {
            source_cell_id: 5,
            ..base
        },
        ReferenceInputs {
            model_path: Some("meshes/b.glb"),
            ..base
        },
        ReferenceInputs {
            model_hash: Some("ac"),
            ..base
        },
        ReferenceInputs {
            position: [1.0, 2.0, 3.5],
            ..base
        },
        ReferenceInputs {
            rotation: [0.0, 0.25, 0.0],
            ..base
        },
        ReferenceInputs { scale: 1.5, ..base },
        ReferenceInputs {
            header_flags: 1,
            ..base
        },
        ReferenceInputs {
            enable_parent_id: Some(7),
            ..base
        },
        ReferenceInputs {
            snow_material: 9,
            ..base
        },
        ReferenceInputs {
            data_hash: Some("ab"),
            ..base
        },
        ReferenceInputs {
            data_hash: None,
            ..base
        },
        ReferenceInputs {
            base_record_hash: Some("bc"),
            ..base
        },
        ReferenceInputs {
            texture_set_hash: Some("cd"),
            ..base
        },
    ];
    for variant in variants {
        assert_ne!(input_digest(&variant), reference);
    }
}

#[test]
fn the_writer_order_follows_the_bytes_and_leaves_them_unchanged() {
    // Two instances equal on the whole sort key can only be told apart by input position. The
    // returned order says which input is at which ordinal, the bytes do not depend on it, and
    // `write_mci` still writes exactly the same file.
    let instance = |transform_x: f32| InstanceInput {
        mesh: MeshRef {
            mesh_hash: [7; 32],
            primitive_index: 0,
        },
        transform: [
            1.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            1.0,
            transform_x,
            0.0,
            0.0,
        ],
        cell_x: 0,
        cell_y: 0,
        stable_id: [0; 16],
        base_form_id: 5,
        snow_material: 0,
        source_worldspace_id: 0,
        world_min: [0.0; 3],
        world_max: [1.0; 3],
    };
    let write = |instances: &[InstanceInput]| {
        let inputs = MciInputs {
            builder_identity: BUILDER,
            worldspace_id: BLOCK_WORLD,
            tier: 16,
            anchor_x: 0,
            anchor_y: 0,
            instances,
        };
        (
            write_mci(&inputs).unwrap(),
            write_mci_ordered(&inputs).unwrap(),
        )
    };
    let twins = [instance(2.0), instance(2.0), instance(1.0)];
    let (plain, (ordered, order)) = write(&twins);
    assert_eq!(plain, ordered);
    // Input 2 sorts first (transform x 1.0), then the twins keep their input order.
    assert_eq!(order, vec![2, 0, 1]);
    let swapped = [instance(2.0), instance(1.0), instance(2.0)];
    let (_, (swapped_bytes, swapped_order)) = write(&swapped);
    assert_eq!(
        swapped_bytes, ordered,
        "the file does not depend on which twin came first"
    );
    assert_eq!(swapped_order, vec![1, 0, 2]);
}

#[test]
fn every_payload_and_the_provenance_must_carry_the_expected_builder() {
    // Provenance and the payload headers agree with each other but not with the converter that
    // is validating: the authority names the builder.
    let fixture = fixture();
    let mut other = truth();
    other.builder_identity = "11".repeat(32);
    assert_eq!(
        check_with(&fixture, &other),
        Err(MappingError::StaleBuilderIdentity)
    );
}

#[test]
fn relabelling_a_reference_in_both_tables_is_not_authoritative() {
    // The same digest under another reference id in the map and the coverage: the tables still
    // agree with each other, but the converter never examined that reference.
    let fixture = fixture();
    for table in ["cluster_instance_map", "cluster_reference_coverage"] {
        run(
            &fixture,
            &format!("UPDATE {table} SET reference_id = 257 WHERE reference_id = 256"),
        );
    }
    check_structure(&fixture).unwrap();
    assert!(matches!(
        check(&fixture),
        Err(MappingError::ScopeMismatch { .. })
    ));
}

#[test]
fn a_primitive_removed_from_every_table_and_the_block_is_caught() {
    // The stage "forgot" primitive 1 of reference 0x200: the mesh rows, the block, the map and the
    // coverage all agree on one primitive, so the emitted store is consistent, and only the
    // source model's own primitive list shows that it is incomplete.
    let kept: Vec<Placed> = placed()
        .into_iter()
        .filter(|p| !(p.reference == 0x200 && p.primitive == 1))
        .collect();
    let fixture = fixture_from(kept);
    check_structure(&fixture).unwrap();
    assert_eq!(
        coverage_state(&fixture.connection, 0x200).unwrap(),
        CoverageState::Complete
    );
    assert_eq!(
        check(&fixture),
        Err(MappingError::ExpectedPrimitivesMismatch {
            reference_id: 0x200,
            mesh_hash: model_hex(2)
        })
    );
}

#[test]
fn the_provenance_must_be_bound_to_the_current_core_identity() {
    let fixture = fixture();
    let mut other = truth();
    other.core_build_identity = Some("ee".repeat(32));
    assert_eq!(
        check_with(&fixture, &other),
        Err(MappingError::StaleBuiltFrom)
    );
    other.core_build_identity = Some(String::new());
    assert_eq!(
        check_with(&fixture, &other),
        Err(MappingError::StaleBuiltFrom),
        "an empty core identity certifies nothing"
    );
    other.core_build_identity = None;
    assert_eq!(
        check_with(&fixture, &other),
        Err(MappingError::StaleBuiltFrom),
        "a dataset without a core identity certifies nothing"
    );
}

#[test]
fn the_declared_scope_must_be_the_examined_scope() {
    let fixture = fixture();
    let mut authority = truth();
    authority.classifier_scope = "{\"fixture\":false}".to_owned();
    assert_eq!(
        check_with(&fixture, &authority),
        Err(MappingError::ScopeDeclarationMismatch)
    );
}

#[test]
fn an_emitted_material_key_must_be_the_authoritys() {
    let fixture = fixture();
    run(
        &fixture,
        &format!(
            "UPDATE cluster_meshes SET material_key = 'other' WHERE mesh_hash = '{}' AND primitive_index = 1",
            model_hex(2)
        ),
    );
    check_structure(&fixture).unwrap();
    assert_eq!(
        check(&fixture),
        Err(MappingError::MaterialMismatch {
            mesh_hash: model_hex(2),
            primitive_index: 1
        })
    );
    // An authority that cannot derive the materials certifies nothing.
    let fixture = self::fixture();
    let mut blind = truth();
    blind.materials.clear();
    assert!(matches!(
        check_with(&fixture, &blind),
        Err(MappingError::MaterialMismatch { .. })
    ));
}

#[test]
fn the_block_of_an_instance_is_the_authoritys_block_not_the_placing_world() {
    // References 0x100 (own world) and 0x600 (a child world joined to the block grid): the
    // records keep their placed identity, but the block that holds them must be the one the
    // block world's LOD origin and the cell give. A block rebuilt under another world, tier or
    // anchor, with its row, map, index and hash updated to match, is caught here.
    let fixture = fixture();
    for reference in [0x100u32, 0x600] {
        for (name, block) in [
            ("world", (BLOCK_WORLD + 1, 16, 0, 0)),
            ("tier", (BLOCK_WORLD, 8, 0, 0)),
            ("anchor x", (BLOCK_WORLD, 16, 1, 0)),
            ("anchor y", (BLOCK_WORLD, 16, 0, -1)),
        ] {
            let mut other = truth();
            other.references.get_mut(&reference).unwrap().block = block;
            assert_eq!(
                check_with(&fixture, &other),
                Err(MappingError::SnapshotMismatch {
                    reference_id: reference,
                    what: "block"
                }),
                "{reference:#x} {name}"
            );
        }
    }
    // The distinction is kept: the child reference's placed world is still its own.
    let snapshot = truth().references[&0x600].clone();
    assert_eq!(snapshot.placed_worldspace_id, CHILD_WORLD);
    assert_eq!(snapshot.block.0, BLOCK_WORLD);
}

#[test]
fn an_emitted_material_source_must_be_the_authoritys_not_only_its_key() {
    // Each edit leaves material_key correct: the reader loads cluster_meshes.model_path and takes
    // the material slot and the render flags from the row, so those are bound too.
    for sql in [
        "UPDATE cluster_meshes SET material_index = 2",
        "UPDATE cluster_meshes SET model_path = 'meshes/other.glb'",
        "UPDATE cluster_meshes SET alpha_mode = 'mask', alpha_cutoff = 0.5",
        "UPDATE cluster_meshes SET double_sided = 1",
    ] {
        let fixture = fixture();
        run(&fixture, sql);
        check_structure(&fixture).unwrap();
        assert!(
            matches!(check(&fixture), Err(MappingError::MaterialMismatch { .. })),
            "{sql}"
        );
    }
    // A source the authority describes with a slot, a mask and two sides matches only that row.
    let fixture = fixture();
    let mut authority = truth();
    for sources in authority.materials.values_mut() {
        for source in sources.values_mut() {
            source.material_index = Some(2);
            source.alpha_mask = true;
            source.alpha_cutoff = Some(0.25);
            source.double_sided = true;
        }
    }
    assert!(matches!(
        check_with(&fixture, &authority),
        Err(MappingError::MaterialMismatch { .. })
    ));
    run(
        &fixture,
        "UPDATE cluster_meshes SET material_index = 2, alpha_mode = 'mask', alpha_cutoff = 0.25,
         double_sided = 1",
    );
    check_with(&fixture, &authority).unwrap();
}
