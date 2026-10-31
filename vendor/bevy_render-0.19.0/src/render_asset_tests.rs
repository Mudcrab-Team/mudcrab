use super::*;
use bevy_ecs::{prelude::Resource, system::RunSystemOnce, world::World};

#[derive(Asset, bevy_reflect::TypePath, Clone)]
struct Source(u32);

struct Prepared(u32);

#[derive(Resource, Default)]
struct Retry(bool);

impl RenderAsset for Prepared {
    type SourceAsset = Source;
    type Param = ResMut<'static, Retry>;

    fn prepare_asset(
        source: Source,
        _: AssetId<Source>,
        retry: &mut SystemParamItem<Self::Param>,
        _: Option<&Self>,
    ) -> Result<Self, PrepareAssetError<Source>> {
        if !retry.0 {
            retry.0 = true;
            return Err(PrepareAssetError::RetryNextUpdate(source));
        }
        Ok(Self(source.0))
    }
}

#[test]
fn same_identity_content_replacement_and_mutation_advance_revision() {
    let mut assets = RenderAssets::<Prepared>::default();
    let id = AssetId::<Source>::default();
    assets.insert(id, Prepared(1));
    let first = assets.revision();
    assets.insert(id, Prepared(2));
    assert!(assets.revision() > first);
    let second = assets.revision();
    assert_eq!(assets.get(id).unwrap().0, 2);
    assert_eq!(assets.revision(), second);
    assets.get_mut(id).unwrap().0 = 3;
    assert!(assets.revision() > second);
    let third = assets.revision();
    assets.remove(id);
    assert!(assets.revision() > third);
}

#[test]
fn successful_retry_publishes_content_revision_in_its_own_frame() {
    let id = AssetId::<Source>::default();
    let mut world = World::new();
    world.init_resource::<RenderAssets<Prepared>>();
    world.init_resource::<ExtractedAssets<Prepared>>();
    world.init_resource::<PrepareNextFrameAssets<Prepared>>();
    world.init_resource::<RenderAssetBytesPerFrameLimiter>();
    world.init_resource::<Retry>();
    world
        .resource_mut::<ExtractedAssets<Prepared>>()
        .extracted
        .push((id, Source(5)));
    let initial = world.resource::<RenderAssets<Prepared>>().revision();
    world.run_system_once(prepare_assets::<Prepared>).unwrap();
    assert_eq!(
        world.resource::<RenderAssets<Prepared>>().revision(),
        initial
    );
    world.run_system_once(prepare_assets::<Prepared>).unwrap();
    assert!(world.resource::<RenderAssets<Prepared>>().revision() > initial);
    assert_eq!(
        world
            .resource::<RenderAssets<Prepared>>()
            .get(id)
            .unwrap()
            .0,
        5
    );
}

#[test]
fn replacement_prepared_assets_cannot_alias_a_retained_revision() {
    let id = AssetId::<Source>::default();
    let mut world = World::new();
    let mut first = RenderAssets::<Prepared>::default();
    let empty = first.revision();
    first.insert(id, Prepared(1));
    let retained = first.revision();
    world.insert_resource(first);
    world.insert_resource(RenderAssets::<Prepared>::default());
    assert!(world.resource::<RenderAssets<Prepared>>().revision() > retained);
    let mut replacement = world.resource_mut::<RenderAssets<Prepared>>();
    replacement.insert(id, Prepared(2));
    assert!(replacement.revision() > retained);
    assert_ne!(replacement.revision(), empty);
}
