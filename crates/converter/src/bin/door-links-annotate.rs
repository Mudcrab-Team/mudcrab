//! Rebuild the optional door projection from a converted database's own winning records.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

fn main() -> Result<()> {
    color_eyre::install()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 1,
        "usage: door-links-annotate <skyrim_world.db> (close readers first)"
    );
    let path = PathBuf::from(&args[0]);
    let root = path
        .parent()
        .filter(|root| !root.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let _lock = shared::asset_lock::AssetLock::acquire_exclusive(root)
        .wrap_err("close asset readers before rebuilding door links")?;
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .wrap_err_with(|| format!("opening {}", path.display()))?;
    let version: u32 = conn.query_row("SELECT version FROM schema_info", [], |row| row.get(0))?;
    ensure!(
        (4..=shared::WORLD_DATABASE_SCHEMA_VERSION).contains(&version),
        "database schema {version} is unsupported"
    );
    let count = converter::esm::doors::rebuild_door_links(&conn)?;
    println!(
        "projected {count} door links from winning records in {}",
        path.display()
    );
    Ok(())
}
