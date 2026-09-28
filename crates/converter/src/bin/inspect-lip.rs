use color_eyre::{Result, eyre::bail};
use converter::lip::{PHONEME_SLOT_BASE, VISEME_NAMES, decode};
use std::{env, path::PathBuf};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let path =
        PathBuf::from(args.next().ok_or_else(|| {
            color_eyre::eyre::eyre!("usage: inspect-lip <lip-file> [frame-index]")
        })?);
    let frame =
        args.next()
            .map(|value| {
                value.to_string_lossy().parse::<usize>().map_err(|_| {
                    color_eyre::eyre::eyre!("frame-index must be a non-negative integer")
                })
            })
            .transpose()?;
    if args.next().is_some() {
        bail!("usage: inspect-lip <lip-file> [frame-index]");
    }

    let lip = decode(&path)?;
    let header = lip.header;
    println!("{}", path.display());
    println!(
        "version={} grid_size={} curves={} declared_frames={} decoded_rows={}",
        header.version,
        header.gridsize,
        header.num_curves,
        header.frames,
        lip.grid.len()
    );
    println!(
        "first={} timing={} payload_offset={} payload_floats={}",
        header.first,
        lip.timing_first
            .map(|first| format!("reliable (first={first}, fps=30)"))
            .unwrap_or_else(|| "ambiguous".to_owned()),
        lip.payload_offset,
        lip.payload_float_count
    );

    if let Some(frame) = frame {
        let Some(row) = lip.grid.get(frame) else {
            bail!(
                "frame {frame} is outside decoded rows 0..{}",
                lip.grid.len()
            );
        };
        let time = lip
            .frame_time(frame)
            .map(|seconds| format!("{seconds:.3}s"))
            .unwrap_or_else(|| "unknown time".to_owned());
        println!("frame {frame} ({time})");
        for (index, name) in VISEME_NAMES.iter().enumerate() {
            println!("  {name:>6}: {:.6}", row[PHONEME_SLOT_BASE + index]);
        }
    } else {
        println!("active visemes (peak > 0.000001):");
        for (index, name) in VISEME_NAMES.iter().enumerate() {
            let peak = lip
                .grid
                .iter()
                .map(|row| row[PHONEME_SLOT_BASE + index].abs())
                .fold(0.0_f32, f32::max);
            if peak > 1e-6 {
                println!("  {name}: peak={peak:.6}");
            }
        }
    }
    Ok(())
}
