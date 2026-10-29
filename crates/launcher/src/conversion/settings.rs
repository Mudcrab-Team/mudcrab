//! Geometry choice for new and resumed conversions.
use bevy::prelude::*;

#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversionSettings {
    pub clustered_geometry: bool,
}

impl ConversionSettings {
    /// Applies the same geometry choice to a new conversion and a resumed conversion.
    pub fn apply(&self, config: &mut converter::PipelineConfig) {
        config.lod_mode = if self.clustered_geometry {
            converter::cluster::LodMode::Cluster
        } else {
            converter::cluster::LodMode::Baked
        };
        config.cluster_terrain = self.clustered_geometry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversion_settings_default_to_baked_and_map_both_choices() {
        let mut config = converter::PipelineConfig::new("data", "output");
        ConversionSettings {
            clustered_geometry: true,
        }
        .apply(&mut config);
        assert_eq!(config.lod_mode, converter::cluster::LodMode::Cluster);
        assert!(config.cluster_terrain);
        ConversionSettings::default().apply(&mut config);
        assert_eq!(config.lod_mode, converter::cluster::LodMode::Baked);
        assert!(!config.cluster_terrain);
    }
}
