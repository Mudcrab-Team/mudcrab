//! The declared scope of the reference mapping: which references the converter examines, and the
//! rules that decide them. The canonical JSON of it is written to the provenance
//! `classifier_scope`, and the authority recomputes it from the same request, so what the package
//! declares is what was examined. A reader interprets a declared scope with
//! [`Scope::from_declared_json`], which accepts only text that is exactly the canonical JSON of a
//! scope this code can produce.

use super::{
    classify::{RESTING_MODEL_TYPES, SnowSource},
    overrides::MINIMUM_REFERENCE_OVERRIDE_TAGS,
};
use crate::{
    cluster::canonical_json,
    cluster_mapping::{DIGEST_VERSION, MATERIAL_RESOLVER},
};
use serde_json::{Value, json};

/// Version of the scope description itself. Version 3 uses the authoritative base
/// marker flag and permits ordinary marker-named models; version 2 packages need rebuilding.
pub const SCOPE_VERSION: u32 = 3;

/// A rectangle of exterior cells of one worldspace, both corners inclusive (cell grid
/// coordinates: the cell a reference's position falls in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellArea {
    pub worldspace_id: u32,
    pub min: (i32, i32),
    pub max: (i32, i32),
}

impl CellArea {
    pub fn contains(&self, worldspace_id: u32, cell_x: i32, cell_y: i32) -> bool {
        worldspace_id == self.worldspace_id
            && (self.min.0..=self.max.0).contains(&cell_x)
            && (self.min.1..=self.max.1).contains(&cell_y)
    }
}

impl ScopeRequest {
    /// The child worldspaces sorted and without duplicates, as the scope declares them.
    pub fn child_worldspaces_sorted(&self) -> Vec<u32> {
        let mut children = self.child_worldspaces.clone();
        children.sort_unstable();
        children.dedup();
        children
    }
}

/// What a run asks to examine, independent of any converter settings type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeRequest {
    /// Trees are in scope as well as statics.
    pub include_trees: bool,
    /// Child worldspaces whose references join their parent's blocks (any order).
    pub child_worldspaces: Vec<u32>,
    /// Only references in this block-world cell rectangle are examined.
    pub area: Option<CellArea>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// Trees are in scope as well as statics.
    pub include_trees: bool,
    /// Child worldspaces whose references join their parent's blocks, sorted and unique.
    pub child_worldspaces: Vec<u32>,
    /// Only references in this block-world cell rectangle are examined.
    pub area: Option<CellArea>,
    /// Where the snow material of a reference is read from in the database; declared because a
    /// reference's recorded snow material depends on it (`none` means every reference has 0).
    pub snow_source: SnowSource,
}

impl Scope {
    /// The scope of `request` over a database whose snow source is `snow_source`.
    pub fn from_request(request: &ScopeRequest, snow_source: SnowSource) -> Self {
        let mut child_worldspaces = request.child_worldspaces.clone();
        child_worldspaces.sort_unstable();
        child_worldspaces.dedup();
        Self {
            include_trees: request.include_trees,
            child_worldspaces,
            area: request.area,
            snow_source,
        }
    }

    /// The request this scope was made from.
    pub fn request(&self) -> ScopeRequest {
        ScopeRequest {
            include_trees: self.include_trees,
            child_worldspaces: self.child_worldspaces.clone(),
            area: self.area,
        }
    }

    /// Whether the reference at `cell` of block worldspace `worldspace_id` is in the area.
    pub fn contains(&self, worldspace_id: u32, cell: (i32, i32)) -> bool {
        self.area
            .is_none_or(|area| area.contains(worldspace_id, cell.0, cell.1))
    }

    /// The canonical JSON written to `cluster_provenance.classifier_scope`.
    pub fn json(&self) -> String {
        let area = self.area.map(|area| {
            json!({
                "worldspace": area.worldspace_id,
                "min": [area.min.0, area.min.1],
                "max": [area.max.0, area.max.1],
            })
        });
        let mut base_types = RESTING_MODEL_TYPES.to_vec();
        if self.include_trees {
            base_types.push("TREE");
        }
        let value = json!({
            "scope_version": SCOPE_VERSION,
            "digest_version": DIGEST_VERSION,
            "references": "exterior references of worldspaces with a LOD origin and of the child worldspaces",
            "child_worldspaces": self.child_worldspaces,
            "base_record_types": base_types,
            "area": area,
            "snow_material_source": self.snow_source.as_str(),
            "reference_override_tags": MINIMUM_REFERENCE_OVERRIDE_TAGS,
            "material_resolver": MATERIAL_RESOLVER,
            "runtime_state": "per_reference_suppression/1",
        });
        String::from_utf8(canonical_json(&value)).expect("canonical JSON is UTF-8")
    }

    /// Interprets the `classifier_scope` a package declares. Only the fields that are settings are
    /// read; the result is accepted only if its canonical JSON is byte for byte the text given, so
    /// a scope of another version, with other rules, tag lists or extra fields, is refused rather
    /// than half understood.
    pub fn from_declared_json(text: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|error| format!("not JSON: {error}"))?;
        let number = |value: &Value| value.as_i64().and_then(|n| i32::try_from(n).ok());
        let field = |name: &str| value.get(name).ok_or_else(|| format!("no {name}"));
        let declared_types = field("base_record_types")?
            .as_array()
            .ok_or("base_record_types is not a list")?;
        let include_trees = declared_types.iter().any(|kind| kind == "TREE");
        let child_worldspaces = field("child_worldspaces")?
            .as_array()
            .ok_or("child_worldspaces is not a list")?
            .iter()
            .map(|id| id.as_u64().and_then(|id| u32::try_from(id).ok()))
            .collect::<Option<Vec<u32>>>()
            .ok_or("a child worldspace is not a form id")?;
        let area = match field("area")? {
            Value::Null => None,
            area => {
                let pair = |name: &str| -> Option<(i32, i32)> {
                    let list = area.get(name)?.as_array()?;
                    Some((number(list.first()?)?, number(list.get(1)?)?))
                };
                Some(CellArea {
                    worldspace_id: area
                        .get("worldspace")
                        .and_then(Value::as_u64)
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or("the area has no worldspace")?,
                    min: pair("min").ok_or("the area has no min")?,
                    max: pair("max").ok_or("the area has no max")?,
                })
            }
        };
        let snow_source = match field("snow_material_source")?.as_str() {
            Some("static_materials") => SnowSource::StaticMaterials,
            Some("statics.material_object") => SnowSource::StaticsColumn,
            Some("none") => SnowSource::None,
            _ => return Err("unknown snow_material_source".into()),
        };
        let scope = Self::from_request(
            &ScopeRequest {
                include_trees,
                child_worldspaces,
                area,
            },
            snow_source,
        );
        if scope.json() != text {
            return Err("the declared scope is not the canonical scope of this recipe".into());
        }
        Ok(scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope::from_request(
            &ScopeRequest {
                include_trees: false,
                child_worldspaces: vec![5],
                area: None,
            },
            SnowSource::StaticMaterials,
        )
    }

    #[test]
    fn historical_filename_marker_policy_is_not_a_supported_scope() {
        let mut legacy: Value = serde_json::from_str(&scope().json()).unwrap();
        legacy["scope_version"] = json!(2);
        let text = String::from_utf8(canonical_json(&legacy)).unwrap();
        assert!(Scope::from_declared_json(&text).is_err());
    }

    #[test]
    fn every_setting_changes_the_declaration() {
        let base = scope().json();
        let mut trees = scope();
        trees.include_trees = true;
        let mut children = scope();
        children.child_worldspaces = vec![5, 6];
        let mut area = scope();
        area.area = Some(CellArea {
            worldspace_id: 60,
            min: (0, 0),
            max: (1, 1),
        });
        let mut snow = scope();
        snow.snow_source = SnowSource::None;
        let mut legacy = scope();
        legacy.snow_source = SnowSource::StaticsColumn;
        for other in [trees, children, area, snow, legacy] {
            assert_ne!(other.json(), base);
        }
        assert_eq!(scope().json(), base);
    }

    #[test]
    fn material_resolution_and_runtime_suppression_are_declared() {
        assert!(scope().json().contains(MATERIAL_RESOLVER));
    }

    #[test]
    fn a_request_is_normalized_and_a_declaration_reads_back_exactly() {
        let request = ScopeRequest {
            include_trees: true,
            child_worldspaces: vec![9, 5, 9],
            area: Some(CellArea {
                worldspace_id: 60,
                min: (-3, 4),
                max: (7, 8),
            }),
        };
        for snow in [
            SnowSource::StaticMaterials,
            SnowSource::StaticsColumn,
            SnowSource::None,
        ] {
            let scope = Scope::from_request(&request, snow);
            assert_eq!(scope.child_worldspaces, [5, 9]);
            let read = Scope::from_declared_json(&scope.json()).unwrap();
            assert_eq!(read, scope);
            assert_eq!(read.request().child_worldspaces, [5, 9]);
        }
    }

    #[test]
    fn a_declaration_that_is_not_this_recipes_exact_text_is_refused() {
        let text = scope().json();
        assert!(Scope::from_declared_json(&text).is_ok());
        for bad in [
            text.replace(
                &format!("\"scope_version\":{SCOPE_VERSION}"),
                "\"scope_version\":1",
            ),
            text.replace(MATERIAL_RESOLVER, "different_material_resolver/1"),
            text.replace("per_reference_suppression/1", "unknown_suppression/1"),
            text.replace("\"STAT\"", "\"NPC_\""),
            text.replace("{", "{\"extra\":1,"),
            text.replace("\"static_materials\"", "\"somewhere\""),
            String::new(),
            "[]".to_owned(),
        ] {
            assert!(Scope::from_declared_json(&bad).is_err(), "{bad}");
        }
    }
}
