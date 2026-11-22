use color_eyre::Result;
use converter::mesh::MeshConverter;
use std::{env, path::PathBuf};
use walkdir::WalkDir;

fn main() -> Result<()> {
    color_eyre::install()?;
    let root = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or_else(|| color_eyre::eyre::eyre!("usage: glb-audit <root>"))?,
    );
    let mut entries = Vec::new();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
        {
            continue;
        }
        match MeshConverter::drawable_bounds(path) {
            Ok(Some(bounds)) => {
                let extent = bounds
                    .min
                    .into_iter()
                    .chain(bounds.max)
                    .map(f32::abs)
                    .fold(0.0_f32, f32::max);
                entries.push((extent, path.to_owned(), Some(bounds), None));
            }
            Ok(None) => entries.push((0.0, path.to_owned(), None, None)),
            Err(error) => entries.push((
                f32::INFINITY,
                path.to_owned(),
                None,
                Some(error.to_string()),
            )),
        }
    }
    entries.sort_by(|left, right| right.0.total_cmp(&left.0));
    for (extent, path, bounds, error) in entries {
        let relative = path.strip_prefix(&root).unwrap_or(&path);
        match (bounds, error) {
            (Some(bounds), _) => println!(
                "extent={extent:.3} min={:?} max={:?} path={}",
                bounds.min,
                bounds.max,
                relative.display()
            ),
            (_, Some(error)) => println!("extent=ERROR path={} error={error}", relative.display()),
            (None, None) => println!("extent=NONE path={}", relative.display()),
        }
    }
    Ok(())
}
