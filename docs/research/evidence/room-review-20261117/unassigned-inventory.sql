-- Read-only inventory for the five freshly reconverted review cells.
-- Potential geometric candidates, not measured culled meshes or native ownership.
WITH membership AS (
    SELECT reference_id, SUM(contained != 0) AS contained
    FROM reference_rooms GROUP BY reference_id
), inventory AS (
    SELECT r.cell_id, c.interior_name,
        m.reference_id IS NULL AS no_membership,
        COALESCE(m.contained, 0) = 0 AS no_contained,
        m.reference_id IS NULL
            AND COALESCE(s.bounds_valid, 0) = 1
            AND COALESCE(s.model_path, '') != ''
            AND (r.header_flags & 2147483648) = 0
            AND COALESCE(b.record_type, '') NOT IN (
                'NPC_', 'LVLN', 'LVLI', 'CREA', 'DOOR', 'ACTI', 'MISC', 'ALCH',
                'AMMO', 'ARMO', 'BOOK', 'INGR', 'KEYM', 'SCRL', 'SLGM', 'WEAP'
            ) AS bounded_candidate,
        (COALESCE(s.flags, 0) & 8388608) != 0
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'markers/%'
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'meshes/markers/%'
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'sky/%'
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'meshes/sky/%'
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'effects/%'
            OR LOWER(REPLACE(COALESCE(s.model_path, ''), CHAR(92), '/')) LIKE 'meshes/effects/%'
            AS model_suppressed
    FROM "references" r
    JOIN room_graph_cells g ON g.cell_id = r.cell_id AND g.complete = 1
    JOIN cells c ON c.id = r.cell_id
    LEFT JOIN membership m ON m.reference_id = r.id
    LEFT JOIN statics s ON s.id = r.base_form_id
    LEFT JOIN records b ON b.form_id = r.base_form_id
    WHERE r.is_exterior = 0 AND r.cell_id IN (86643, 91555, 225758, 228550, 768815)
)
SELECT cell_id, interior_name, COUNT(*) AS references_total,
    SUM(no_contained) AS no_containing_room,
    SUM(no_membership) AS no_containing_or_overlap_room,
    SUM(bounded_candidate) AS bounded_candidates_before_model_suppression,
    SUM(bounded_candidate AND NOT model_suppressed) AS bounded_candidates_after_model_suppression
FROM inventory GROUP BY cell_id ORDER BY cell_id;
