//! CPU-only production grass diagnostics. Input caches and CSVs stay outside the repository.
//! Reference frame decoding follows chasmlol/spud (MIT); see ../src/grass/SPUD-MIT.txt.
use engine::{
    config::EngineConfig,
    grass::generation::{Placement, generate_observed},
    world::grass_cache::GrassCache,
};
use std::{
    error::Error,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};

/// Shared decoded fields plus diagnostic production packing, without proprietary source bytes.
const HEADER: &str = "cell_x,cell_y,ordinal,form_id,x_su,y_su,z_su,scale_x,scale_y,scale_z,brightness,r00,r01,r02,r10,r11,r12,r20,r21,r22,height_delta,stage,obstructed,packed_x_su,packed_y_su,packed_z_su,packed_scale_x,packed_scale_y,packed_scale_z,packed_brightness,packed_r00,packed_r01,packed_r02,packed_r10,packed_r11,packed_r12,packed_r20,packed_r21,packed_r22";

/// Convert the full u32 word as the reference does, without reducing its entropy.
fn signed(word: u32) -> f32 {
    word as f32 * 4.656_613e-10 - 1.0
}

/// Quantize to binary16 for direct comparison with the exported reference shader inputs.
fn half(value: f32) -> f32 {
    let exponent = ((value.abs().to_bits() >> 23) & 255) as i32 - 127;
    let step = 2.0f32.powi((exponent - 10).max(-24));
    (value / step).round_ties_even() * step
}

/// Normalize authored or packed normals, retaining the reference's zero vector.
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.map(|x| if len > 1e-6 { x / len } else { 0.0 })
}

/// Return a row-major Creation-space orientation matrix; scale is reported separately.
fn frame(n: [f32; 3], sn: f32, cs: f32, slope: bool) -> [f32; 9] {
    let (p, q, n) = if slope {
        let t = normalize(if n[0].abs() <= n[1].abs() && n[0].abs() <= n[2].abs() {
            [0.0, -n[2], n[1]]
        } else if n[1].abs() <= n[0].abs() && n[1].abs() <= n[2].abs() {
            [-n[2], 0.0, n[0]]
        } else {
            [-n[1], n[0], 0.0]
        });
        let b = [
            n[2] * t[1] - n[1] * t[2],
            n[0] * t[2] - n[2] * t[0],
            n[1] * t[0] - n[0] * t[1],
        ];
        (
            std::array::from_fn::<_, 3, _>(|i| cs * t[i] + sn * b[i]),
            std::array::from_fn::<_, 3, _>(|i| sn * t[i] - cs * b[i]),
            n,
        )
    } else {
        ([sn, cs, 0.0], [-cs, sn, 0.0], [0.0, 0.0, 1.0])
    };
    [p[0], q[0], n[0], p[1], q[1], n[1], p[2], q[2], n[2]]
}

/// Write original selection plus decoded production packing, including mask rejections.
fn row(
    out: &mut impl Write,
    cache: &GrassCache,
    grid: [i32; 2],
    ordinal: usize,
    p: Placement,
) -> std::io::Result<()> {
    let rule = cache.species(p.form_id).expect("observed rule exists");
    let slope = rule.flags & 4 != 0;
    let uniform = rule.flags & 2 != 0;
    let sn = signed(p.appearance[1]);
    let rotation = frame(p.normal, sn, (1.0 - sn * sn).max(0.0).sqrt(), slope).map(half);
    let delta = half(signed(p.appearance[2]) * f32::from(rule.height_range));
    let scale = [
        if uniform { 1.0 + delta } else { 1.0 },
        if uniform { 1.0 + delta } else { 1.0 },
        1.0 + delta,
    ];
    let factor = (p.appearance[0] as f32 * 1.164_153_2e-10 - 0.25) * f32::from(rule.color_range)
        + (1.0 - f32::from(rule.color_range));
    let brightness = p.luma * factor;
    let brightness = half(if brightness >= 1.0 {
        0.99
    } else {
        brightness.max(0.0)
    });
    let packed_pos = [
        grid[0] as f32 * 4096.0 + f32::from(p.instance.position[0]) * (4096.0 / 65535.0),
        grid[1] as f32 * 4096.0 + f32::from(p.instance.position[1]) * (4096.0 / 65535.0),
        p.height_min + f32::from(p.instance.position[2]) * p.height_span / 65535.0,
    ];
    let packed_s = f32::from(p.instance.scale) * (2.0 / 255.0);
    let packed_scale = [
        if uniform { packed_s } else { 1.0 },
        if uniform { packed_s } else { 1.0 },
        packed_s,
    ];
    let angle = f32::from(p.instance.rotation) * (std::f32::consts::TAU / 256.0);
    let normal = normalize(p.instance.normal.map(f32::from));
    let packed_rotation = if slope {
        frame(normal, -angle.sin(), angle.cos(), true)
    } else {
        frame(normal, angle.cos(), angle.sin(), false)
    };
    write!(out, "{},{},{ordinal},{:08x}", grid[0], grid[1], p.form_id)?;
    for v in p
        .position
        .into_iter()
        .chain(scale)
        .chain([brightness])
        .chain(rotation)
    {
        write!(out, ",{v}")?;
    }
    write!(
        out,
        ",{delta},{},{}",
        if p.is_base { "base" } else { "layer" },
        u8::from(p.obstructed)
    )?;
    for v in packed_pos
        .into_iter()
        .chain(packed_scale)
        .chain([f32::from(p.instance.color) / 255.0])
        .chain(packed_rotation)
    {
        write!(out, ",{v}")?;
    }
    writeln!(out)
}

/// Dump a caller-selected set of cells; no app, graphics device or game executable is created.
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: grass_dump GRASS_CACHE REQUESTED_CELLS.csv OUTPUT_DIRECTORY".into());
    }
    let cache = GrassCache::open(Path::new(&args[1]))?;
    let out = Path::new(&args[3]);
    fs::create_dir_all(out)?;
    let config = EngineConfig::default();
    let mut status = BufWriter::new(File::create(out.join("cells.csv"))?);
    writeln!(
        status,
        "cell_x,cell_y,status,generated_count,obstructed_count,production_count"
    )?;
    for line in BufReader::new(File::open(&args[2])?).lines() {
        let line = line?;
        let mut fields = line.trim().split(',');
        let x = fields.next().ok_or("missing cell x")?;
        let y = fields.next().ok_or("missing cell y")?;
        let grid = [x.parse::<i32>()?, y.parse::<i32>()?];
        // Optional explicit world supports references that merge child worlds into their parent.
        let world = fields
            .next()
            .map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16))
            .transpose()?
            .unwrap_or(0x3c);
        let mut csv = BufWriter::new(File::create(
            out.join(format!("cell_{}_{}.csv", grid[0], grid[1])),
        )?);
        writeln!(csv, "{HEADER}")?;
        let Some(id) = cache.cell_at(world, grid[0], grid[1]) else {
            // The grass cache is sparse: conversion omits cells without grass nominations.
            // This is also generate()'s production empty behavior. Keep absence explicit.
            eprintln!(
                "warning: no grass-cache entry {},{} world {world:08x}; production is empty",
                grid[0], grid[1]
            );
            writeln!(status, "{},{},absent-cache,0,0,0", grid[0], grid[1])?;
            csv.flush()?;
            continue;
        };
        let mut ordinal = 0;
        let mut blocked = 0;
        let mut error = Ok(());
        let generated = generate_observed(&cache, id, &config, |p| {
            blocked += usize::from(p.obstructed);
            if error.is_ok() {
                error = row(&mut csv, &cache, grid, ordinal, p);
            }
            ordinal += 1;
        });
        error?;
        csv.flush()?;
        let production: usize = generated.batches.iter().map(|b| b.instances.len()).sum();
        assert_eq!(
            production,
            ordinal - blocked,
            "observer must account for every production blade"
        );
        writeln!(
            status,
            "{},{},present,{ordinal},{blocked},{production}",
            grid[0], grid[1]
        )?;
        eprintln!(
            "{},{} world {world:08x} cell {id:08x}: {ordinal} candidates, {blocked} obstructed",
            grid[0], grid[1]
        );
    }
    status.flush()?;
    Ok(())
}
