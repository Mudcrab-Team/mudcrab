//! Worn outfit attachment for spawned actors.
//!
//! The catalog's default outfit is a list of converted armor and clothing scenes, each skinned
//! against the same shared humanoid skeleton as the composed body. Once the body scene is ready,
//! every part is loaded as its own scene and its skinned meshes get their joint lists rewritten to
//! the body's bone entities of the same [`Name`] (see [`rehost_scene_skins`], shared with the
//! FaceGen head), so the part follows the animated body.
//!
//! A part that covers the body, hands or feet slot hides the composed body mesh of that region, so
//! the underwear skin does not show through the clothing. The body meshes are found by name while
//! the body is the only scene under the actor; the composed body names its hand and foot shapes
//! `Hand*` and `Foot*`, and every other skinned shape is the body.
//!
//! A part that fails to load, has no skin, or names a joint the body lacks is reported once and
//! left out. It never hides body regions, so a failure leaves skin, not a hole.

use crate::actor_animation::{ActorBodyReady, animation_targets};
use crate::actor_head::{RehostedSkins, rehost_scene_skins};
use crate::actors::{Actor, ActorOutfit};
use bevy::{
    asset::LoadState,
    gltf::GltfAssetLabel,
    mesh::skinning::SkinnedMesh,
    prelude::*,
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady},
};
use shared::actor_outfits::OutfitPart;
use std::{collections::HashMap, fmt};

/// A region of the composed body that a worn part can cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyRegion {
    Body,
    Hands,
    Feet,
}

impl BodyRegion {
    /// The region a composed body shape belongs to, from its node name.
    pub fn of_shape(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("hand") {
            Self::Hands
        } else if lower.starts_with("foot") || lower.starts_with("feet") {
            Self::Feet
        } else {
            Self::Body
        }
    }

    fn covered_by(self, part: &OutfitPart) -> bool {
        match self {
            Self::Body => part.covers_body(),
            Self::Hands => part.covers_hands(),
            Self::Feet => part.covers_feet(),
        }
    }
}

/// One outfit scene the actor asked for and the asset server is loading.
#[derive(Debug, Clone)]
struct OutfitRequest {
    asset: Handle<WorldAsset>,
    part: OutfitPart,
}

/// The outfit scenes still loading for an actor.
#[derive(Component, Debug, Clone)]
struct ActorOutfitRequests(Vec<OutfitRequest>);

/// The body bones and body meshes an outfit binds to, captured while the body scene is the only
/// scene under the actor so the name walk cannot find a worn part's own nodes.
#[derive(Component, Debug, Clone)]
struct ActorOutfitSkeleton {
    bones: HashMap<String, Entity>,
    body_meshes: Vec<(BodyRegion, Entity)>,
}

/// One instantiated outfit scene waiting for its joints to be rehosted.
#[derive(Component, Debug, Clone)]
struct ActorOutfitInstance {
    actor: Entity,
    base_form_id: u32,
    part: OutfitPart,
}

/// An actor whose outfit decision is final: requested, skipped, or failed.
#[derive(Component, Debug, Clone, Copy)]
struct ActorOutfitResolved;

/// A typed reason a worn part was not attached to its actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorOutfitDiagnostic {
    /// The body scene has no animation root, so no bone can be matched.
    MissingSkeleton { base_form_id: u32 },
    /// The part's converted scene failed to load.
    MissingAsset { base_form_id: u32, path: String },
    /// The part is not skinned, so it has no joints to bind and would draw at the origin.
    Skinless { base_form_id: u32, path: String },
    /// A part joint has no body bone of the same name.
    UnresolvedJoints {
        base_form_id: u32,
        path: String,
        joints: usize,
    },
}

impl ActorOutfitDiagnostic {
    /// The stable identifier of this diagnostic.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingSkeleton { .. } => "actor_outfit_skeleton_missing",
            Self::MissingAsset { .. } => "actor_outfit_asset_missing",
            Self::Skinless { .. } => "actor_outfit_part_skinless",
            Self::UnresolvedJoints { .. } => "actor_outfit_joint_missing",
        }
    }

    pub fn base_form_id(&self) -> u32 {
        match self {
            Self::MissingSkeleton { base_form_id }
            | Self::MissingAsset { base_form_id, .. }
            | Self::Skinless { base_form_id, .. }
            | Self::UnresolvedJoints { base_form_id, .. } => *base_form_id,
        }
    }
}

impl fmt::Display for ActorOutfitDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSkeleton { base_form_id } => {
                write!(
                    formatter,
                    "NPC_ {base_form_id:08X}: the body has no skeleton"
                )
            }
            Self::MissingAsset { base_form_id, path } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: outfit part `{path}` has no converted asset"
            ),
            Self::Skinless { base_form_id, path } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: outfit part `{path}` has no skin"
            ),
            Self::UnresolvedJoints {
                base_form_id,
                path,
                joints,
            } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: {joints} joint(s) of outfit part `{path}` have no body bone"
            ),
        }
    }
}

impl std::error::Error for ActorOutfitDiagnostic {}

fn report(diagnostic: &ActorOutfitDiagnostic) {
    warn!(
        code = diagnostic.code(),
        form_id = format_args!("{:08X}", diagnostic.base_form_id()),
        "{diagnostic}"
    );
}

/// Attaches catalog outfit parts to their actors' body skeletons.
pub struct ActorOutfitPlugin;

impl Plugin for ActorOutfitPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(attach_ready_outfit_parts).add_systems(
            Update,
            (queue_actor_outfits, spawn_ready_outfit_scenes).chain(),
        );
    }
}

type PendingOutfitActors<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Actor, &'static ActorOutfit),
    (With<ActorBodyReady>, Without<ActorOutfitResolved>),
>;

/// Captures the body skeleton and meshes an outfit binds to and asks the asset server for each
/// part's scene.
fn queue_actor_outfits(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    names: Query<&Name>,
    children: Query<&Children>,
    skinned: Query<(), With<SkinnedMesh>>,
    actors: PendingOutfitActors,
) {
    for (actor, actor_data, outfit) in &actors {
        let Ok(targets) = animation_targets(actor, &names, &children) else {
            report(&ActorOutfitDiagnostic::MissingSkeleton {
                base_form_id: actor_data.base_form_id,
            });
            commands.entity(actor).insert(ActorOutfitResolved);
            continue;
        };
        let bones: HashMap<String, Entity> = targets
            .into_iter()
            .filter_map(|target| {
                names
                    .get(target.entity)
                    .ok()
                    .map(|name| (name.as_str().to_owned(), target.entity))
            })
            .collect();
        let body_meshes = children
            .iter_descendants(actor)
            .filter(|entity| skinned.contains(*entity))
            .filter_map(|entity| {
                names
                    .get(entity)
                    .ok()
                    .map(|name| (BodyRegion::of_shape(name.as_str()), entity))
            })
            .collect();
        let requests = outfit
            .0
            .iter()
            .map(|part| OutfitRequest {
                asset: asset_server.load(GltfAssetLabel::Scene(0).from_asset(part.scene.clone())),
                part: part.clone(),
            })
            .collect();
        commands.entity(actor).insert((
            ActorOutfitRequests(requests),
            ActorOutfitSkeleton { bones, body_meshes },
            ActorOutfitResolved,
        ));
    }
}

/// Instantiates each part scene once the asset server has it, and reports a failed load once.
fn spawn_ready_outfit_scenes(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut actors: Query<(Entity, &Actor, &mut ActorOutfitRequests)>,
) {
    for (actor, actor_data, mut requests) in &mut actors {
        requests.0.retain(
            |request| match asset_server.load_state(request.asset.id()) {
                LoadState::Loaded => {
                    commands.spawn((
                        Name::new(format!(
                            "Outfit {:08X} {:08X}",
                            actor_data.base_form_id, request.part.armo_id
                        )),
                        ActorOutfitInstance {
                            actor,
                            base_form_id: actor_data.base_form_id,
                            part: request.part.clone(),
                        },
                        WorldAssetRoot(request.asset.clone()),
                        ChildOf(actor),
                    ));
                    false
                }
                LoadState::Failed(error) => {
                    let diagnostic = ActorOutfitDiagnostic::MissingAsset {
                        base_form_id: actor_data.base_form_id,
                        path: request.part.scene.clone(),
                    };
                    warn!(
                        code = diagnostic.code(),
                        form_id = format_args!("{:08X}", diagnostic.base_form_id()),
                        %error,
                        "{diagnostic}: {error}"
                    );
                    false
                }
                LoadState::NotLoaded | LoadState::Loading => true,
            },
        );
        if requests.0.is_empty() {
            commands.entity(actor).remove::<ActorOutfitRequests>();
        }
    }
}

/// Rehosts a ready part's skins onto the actor's body bones, then hides the body regions the part
/// covers. A part that cannot bind is removed and covers nothing.
fn attach_ready_outfit_parts(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    names: Query<&Name>,
    children: Query<&Children>,
    skeletons: Query<&ActorOutfitSkeleton>,
    instances: Query<&ActorOutfitInstance>,
    skinned: Query<&SkinnedMesh>,
) {
    let Ok(instance) = instances.get(ready.entity) else {
        return;
    };
    let Ok(skeleton) = skeletons.get(instance.actor) else {
        commands.entity(ready.entity).despawn();
        return;
    };
    let path = instance.part.scene.clone();
    let rehosted =
        match rehost_scene_skins(ready.entity, &skeleton.bones, &names, &children, &skinned) {
            RehostedSkins::Ready(rehosted) => rehosted,
            RehostedSkins::Skinless => {
                report(&ActorOutfitDiagnostic::Skinless {
                    base_form_id: instance.base_form_id,
                    path,
                });
                commands.entity(ready.entity).despawn();
                return;
            }
            RehostedSkins::Unresolved { joints } => {
                report(&ActorOutfitDiagnostic::UnresolvedJoints {
                    base_form_id: instance.base_form_id,
                    path,
                    joints,
                });
                commands.entity(ready.entity).despawn();
                return;
            }
        };
    for (entity, skin) in rehosted {
        commands.entity(entity).insert(skin);
    }
    for (region, mesh) in &skeleton.body_meshes {
        if region.covered_by(&instance.part) {
            commands.entity(*mesh).insert(Visibility::Hidden);
        }
    }
}

#[cfg(test)]
mod tests;
