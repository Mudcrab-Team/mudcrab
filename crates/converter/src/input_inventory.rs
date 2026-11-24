//! Count policy exclusions without changing selection or retaining source path lists.
use std::{collections::BTreeMap, path::Path};

/// Group one omitted physical input by a stable lowercase extension.
pub(crate) fn record_ignored(counts: &mut BTreeMap<String, u64>, path: &Path) {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
        .unwrap_or_else(|| "<none>".into());
    *counts.entry(extension).or_default() += 1;
}

/// Count a discovered loose input unless a pipeline stage accepts its kind.
/// Reuse the archive superset policy; stages that publish additional paths must also
/// update the counter's supported set and its regression test.
/// The optional PC control files under `interface/controls/pc` are published by
/// `controls.rs`, so their own paths are not ignored inputs.
/// Keep the metadata extensions in sync with `esm::plugins_txt::plugin_name`
/// and archive discovery in `pipeline::AssetPipeline::run_into`.
pub(crate) fn count_loose(counts: &mut BTreeMap<String, u64>, relative: &Path) {
    let lower = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let path = Path::new(&lower);
    let metadata = matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("esm" | "esp" | "esl" | "bsa" | "ba2")
    ) || matches!(
        lower.as_str(),
        "plugins.txt" | "loadorder.txt" | "skyrim.ccc"
    );
    if !metadata
        && !crate::controls::owns_input(path)
        && !crate::cache::IngestionSelection::ConverterInputsV1.includes(path)
    {
        record_ignored(counts, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Case, duplicate occurrences, extensionless inputs and metadata stay explicit.
    #[test]
    fn ignored_input_groups_match_policy_and_count_occurrences() {
        let mut counts = BTreeMap::new();
        for path in [
            "sound/a.WAV",
            "sound/b.wav",
            "interface/a.swf",
            "README",
            "textures/a.dds",
            "Skyrim.esm",
            "Skyrim - Textures.bsa",
            "plugins.txt",
        ] {
            count_loose(&mut counts, Path::new(path));
        }
        assert_eq!(
            counts,
            BTreeMap::from([("<none>".into(), 1), ("swf".into(), 1), ("wav".into(), 2),])
        );
    }
}
