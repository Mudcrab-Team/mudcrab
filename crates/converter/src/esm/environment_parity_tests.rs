//! Both record readers publish the same cell and worldspace environment rows (#269 gap 1).

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::{Connection, types::Value};
use std::{fs, path::Path};

const TEMPLATE: u32 = 0x0900;
const IMAGE_SPACE: u32 = 0x0901;
const ACOUSTICS: u32 = 0x0902;
const MUSIC: u32 = 0x0903;
const REGION: u32 = 0x0904;
const LOCATION: u32 = 0x0905;
const ZONE: u32 = 0x0906;
const FACTION: u32 = 0x0907;
const WATER: u32 = 0x0908;
const CLIMATE: u32 = 0x0909;
const WORLD: u32 = 0x0A00;
const LIT_CELL: u32 = 0x0A01;
const SHORT_CELL: u32 = 0x0A02;
/// The mod's own music, authored with its master count (1) as the file index.
const MOD_MUSIC_AUTHORED: u32 = 0x0100_0800;
/// The same record in load-order numbering: the mod is the third plugin.
const MOD_MUSIC: u32 = 0x0200_0800;
const WEATHER: u32 = 0x0B00;
const RAIN: u32 = 0x0B01;
const GLOBAL: u32 = 0x0B02;
const SKY_STATIC: u32 = 0x0B03;
const PRECIPITATION: u32 = 0x0B04;
/// The mod's own image space and weather, authored and in load-order numbering.
const MOD_IMAGE_SPACE_AUTHORED: u32 = 0x0100_0801;
const MOD_IMAGE_SPACE: u32 = 0x0200_0801;
const MOD_WEATHER_AUTHORED: u32 = 0x0100_0802;
const MOD_WEATHER: u32 = 0x0200_0802;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
    record_with_flags(tag, id, 0, payload)
}

fn record_with_flags(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(flags.to_le_bytes());
    framed.extend(id.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0u16.to_le_bytes());
    framed.extend(payload);
    framed
}

fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(label.to_le_bytes());
    framed.extend(kind.to_le_bytes());
    framed.extend([0; 8]);
    framed.extend(payload);
    framed
}

fn top(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    group(0, u32::from_le_bytes(*tag), payload)
}

fn header(flags: u32, masters: &[&str]) -> Vec<u8> {
    let hedr = [
        1.7f32.to_le_bytes(),
        0u32.to_le_bytes(),
        0x800u32.to_le_bytes(),
    ]
    .concat();
    let mut payload = sub(b"HEDR", &hedr);
    for master in masters {
        payload.extend(sub(b"MAST", &[master.as_bytes(), &[0]].concat()));
        payload.extend(sub(b"DATA", &[0; 8]));
    }
    record_with_flags(b"TES4", 0, flags, &payload)
}

fn named(tag: &[u8; 4], id: u32) -> Vec<u8> {
    let editor_id = format!("{}{id:X}\0", String::from_utf8_lossy(tag));
    top(tag, &record(tag, id, &sub(b"EDID", editor_id.as_bytes())))
}

fn le(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// A full 92-byte `XCLL` whose every byte is distinct, so a shifted field cannot pass.
fn xcll() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend([10, 11, 12, 0, 20, 21, 22, 0, 30, 31, 32, 0]);
    bytes.extend(floats(&[64.0, 4096.0]));
    bytes.extend(45i32.to_le_bytes());
    bytes.extend((-30i32).to_le_bytes());
    bytes.extend(floats(&[0.75, 9000.0, 1.5]));
    for colour in 0..7u8 {
        bytes.extend([100 + colour, 110 + colour, 120 + colour, 0]);
    }
    bytes.extend(floats(&[2.0]));
    bytes.extend([40, 41, 42, 0]);
    bytes.extend(floats(&[0.9, 3000.0, 4000.0]));
    bytes.extend(0x0000_0401u32.to_le_bytes());
    assert_eq!(bytes.len(), 92);
    bytes
}

fn interior(id: u32, payload: &[u8]) -> Vec<u8> {
    record(
        b"CELL",
        id,
        &[
            sub(b"EDID", format!("Cell{id:X}\0").as_bytes()),
            payload.to_vec(),
        ]
        .concat(),
    )
}

/// A 32-byte directional ambient block whose bytes start at `base`.
fn dalc(base: u8) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..7u8)
        .flat_map(|colour| {
            [
                base + colour * 3,
                base + colour * 3 + 1,
                base + colour * 3 + 2,
                0,
            ]
        })
        .collect();
    bytes.extend(floats(&[1.0 + f32::from(base)]));
    bytes
}

/// A weather with fog, 14 `NAM0` colour types, image spaces and four `DALC`s.
fn weather(id: u32, editor_id: &str, image_space: u32) -> Vec<u8> {
    let mut data = vec![0u8; 19];
    data[0] = 30; // wind speed
    data[11] = 0x02; // cloudy
    data[12..15].copy_from_slice(&[200, 210, 220]);
    let nam0: Vec<u8> = (0..14u8)
        .flat_map(|colour| {
            (0..4u8).flat_map(move |time| [colour * 10 + time, 100 + time, 200 - colour, 0])
        })
        .collect();
    let mut payload = [
        sub(b"EDID", format!("{editor_id}\0").as_bytes()),
        sub(b"MNAM", &le(PRECIPITATION)),
        sub(b"NAM0", &nam0),
        sub(
            b"FNAM",
            &floats(&[1024.0, 200_000.0, 512.0, 100_000.0, 1.0, 2.0, 0.75, 0.5]),
        ),
        sub(b"DATA", &data),
        sub(b"NAM1", &5u32.to_le_bytes()),
        sub(b"TNAM", &le(SKY_STATIC)),
        sub(
            b"IMSP",
            &[le(image_space), le(image_space), le(IMAGE_SPACE), le(0)].concat(),
        ),
    ]
    .concat();
    for base in [10u8, 50, 90, 130] {
        payload.extend(sub(b"DALC", &dalc(base)));
    }
    record(b"WTHR", id, &payload)
}

fn image_space(id: u32, saturation: f32) -> Vec<u8> {
    record(
        b"IMGS",
        id,
        &[
            sub(b"EDID", format!("Imgs{id:X}\0").as_bytes()),
            sub(
                b"HNAM",
                &floats(&[0.9, 7.0, 0.6, 0.3, 0.1, 2.0, 1.8, 1.5, 0.7]),
            ),
            sub(b"CNAM", &floats(&[saturation, 1.135, 1.4])),
            sub(b"TNAM", &floats(&[0.42, 0.894, 0.839, 0.773])),
            sub(
                b"DNAM",
                &[
                    floats(&[0.5, 400.0, 900.0]),
                    vec![0, 0],
                    16672u16.to_le_bytes().to_vec(),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
}

fn master_plugin() -> Vec<u8> {
    let lit = interior(
        LIT_CELL,
        &[
            sub(b"DATA", &0x0081u16.to_le_bytes()),
            sub(b"XCLL", &xcll()),
            sub(b"LTMP", &le(TEMPLATE)),
            sub(b"XCLW", &floats(&[12.5])),
            sub(b"XCLR", &[le(REGION), le(0xDEAD)].concat()),
            sub(b"XLCN", &le(LOCATION)),
            sub(b"XCWT", &le(WATER)),
            sub(b"XOWN", &le(FACTION)),
            sub(b"XRNK", &2i32.to_le_bytes()),
            sub(b"XCCM", &le(REGION)),
            sub(b"XCAS", &le(ACOUSTICS)),
            sub(b"XEZN", &le(ZONE)),
            sub(b"XCMO", &le(MUSIC)),
            sub(b"XCIM", &le(IMAGE_SPACE)),
        ]
        .concat(),
    );
    // A 72-byte XCLL and a lighting template that names an image space, not a template.
    let short = interior(
        SHORT_CELL,
        &[
            sub(b"DATA", &[1, 0]),
            sub(b"XCLL", &xcll()[..72]),
            sub(b"LTMP", &le(IMAGE_SPACE)),
        ]
        .concat(),
    );
    let world = record(
        b"WRLD",
        WORLD,
        &[
            sub(b"EDID", b"World\0"),
            sub(
                b"WCTR",
                &[3i16.to_le_bytes(), (-4i16).to_le_bytes()].concat(),
            ),
            sub(b"LTMP", &le(TEMPLATE)),
            sub(b"CNAM", &le(CLIMATE)),
            sub(b"NAM2", &le(WATER)),
            sub(b"NAM3", &le(WATER)),
            sub(b"NAM4", &floats(&[-14000.0])),
            sub(b"DNAM", &floats(&[-2048.0, 0.0])),
            sub(b"NAMA", &floats(&[1.25])),
            sub(b"DATA", &[0x20]),
            sub(b"NAM0", &floats(&[-1000.0, -2000.0])),
            sub(b"NAM9", &floats(&[3000.0, 4000.0])),
            sub(b"ZNAM", &le(MUSIC)),
        ]
        .concat(),
    );
    [
        header(1, &[]),
        top(
            b"LGTM",
            &record(
                b"LGTM",
                TEMPLATE,
                &[
                    sub(b"EDID", b"Template\0"),
                    sub(b"DATA", &[&xcll()[..88], &[0; 4]].concat()),
                    sub(b"DALC", &dalc(170)),
                ]
                .concat(),
            ),
        ),
        named(b"GLOB", GLOBAL),
        named(b"STAT", SKY_STATIC),
        named(b"SPGD", PRECIPITATION),
        top(
            b"WTHR",
            &[
                weather(WEATHER, "Clear", IMAGE_SPACE),
                weather(RAIN, "Rain", IMAGE_SPACE),
            ]
            .concat(),
        ),
        top(b"IMGS", &image_space(IMAGE_SPACE, 1.5)),
        named(b"ASPC", ACOUSTICS),
        named(b"MUSC", MUSIC),
        top(
            b"REGN",
            &record(
                b"REGN",
                REGION,
                &[
                    sub(b"EDID", b"Region\0"),
                    sub(b"RDAT", &[3u32.to_le_bytes(), [0, 50, 0, 0]].concat()),
                    sub(
                        b"RDWT",
                        &[
                            [le(RAIN), 100u32.to_le_bytes(), le(0xBEEF)].concat(),
                            [le(WEATHER), 0x8000_0000u32.to_le_bytes(), le(GLOBAL)].concat(),
                            [le(RAIN), u32::MAX.to_le_bytes(), le(0xBEEF)].concat(),
                        ]
                        .concat(),
                    ),
                ]
                .concat(),
            ),
        ),
        named(b"LCTN", LOCATION),
        named(b"ECZN", ZONE),
        named(b"FACT", FACTION),
        named(b"WATR", WATER),
        top(
            b"CLMT",
            &record(
                b"CLMT",
                CLIMATE,
                &[
                    sub(b"EDID", b"Climate\0"),
                    sub(
                        b"WLST",
                        &[
                            le(WEATHER),
                            70i32.to_le_bytes(),
                            le(0),
                            le(RAIN),
                            30i32.to_le_bytes(),
                            le(GLOBAL),
                        ]
                        .concat(),
                    ),
                    sub(b"FNAM", b"sky\\sun.dds\0"),
                    sub(b"TNAM", &[30, 42, 108, 120, 50, 3]),
                ]
                .concat(),
            ),
        ),
        top(b"WRLD", &world),
        top(b"CELL", &group(2, 0, &group(3, 0, &[lit, short].concat()))),
    ]
    .concat()
}

/// A mod after an unrelated master: its own music, and the worldspace now playing it.
fn mod_plugin() -> Vec<u8> {
    let world = record(
        b"WRLD",
        WORLD,
        &[
            sub(b"EDID", b"World\0"),
            sub(b"CNAM", &le(CLIMATE)),
            sub(b"ZNAM", &le(MOD_MUSIC_AUTHORED)),
        ]
        .concat(),
    );
    let climate = record(
        b"CLMT",
        CLIMATE,
        &[
            sub(b"EDID", b"Climate\0"),
            sub(
                b"WLST",
                &[
                    [le(MOD_WEATHER_AUTHORED), (-1i32).to_le_bytes(), le(GLOBAL)].concat(),
                    [le(WEATHER), i32::MIN.to_le_bytes(), le(GLOBAL)].concat(),
                    [le(RAIN), 100i32.to_le_bytes(), le(GLOBAL)].concat(),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    [
        header(0, &["Skyrim.esm"]),
        named(b"MUSC", MOD_MUSIC_AUTHORED),
        top(b"IMGS", &image_space(MOD_IMAGE_SPACE_AUTHORED, 0.5)),
        top(
            b"WTHR",
            &weather(MOD_WEATHER_AUTHORED, "ModWeather", MOD_IMAGE_SPACE_AUTHORED),
        ),
        top(b"CLMT", &climate),
        top(b"WRLD", &world),
    ]
    .concat()
}

type Rows = Vec<(String, Vec<Value>)>;

fn rows(conn: &Connection) -> Rows {
    let mut rows = Rows::new();
    for (table, key) in [
        ("cell_environment", "cell_id"),
        ("cell_regions", "cell_id, region_id"),
        ("cell_lighting", "cell_id"),
        ("worldspace_environment", "worldspace_id"),
        ("lighting_templates", "id"),
        ("weathers", "id"),
        ("weather_colors", "weather_id, color_type, time_of_day"),
        ("weather_directional_ambient", "weather_id, time_of_day"),
        ("climates", "id"),
        ("climate_weathers", "climate_id, position"),
        ("region_weathers", "region_id, position"),
        ("image_spaces", "id"),
    ] {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {key}"))
            .unwrap();
        let columns = statement.column_count();
        rows.extend(
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| (table.to_owned(), values))
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

fn publish(reader: RecordReader, root: &Path) -> Connection {
    let paths = ["Skyrim.esm", "Other.esm", "Mod.esp"].map(|name| root.join(name));
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(&paths, &database, reader, root).unwrap();
    Connection::open(database).unwrap()
}

fn get<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn both_record_readers_publish_the_same_environment() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Other.esm"), header(1, &[])).unwrap();
    fs::write(root.path().join("Mod.esp"), mod_plugin()).unwrap();

    let legacy = publish(RecordReader::Legacy, root.path());
    let inhouse = publish(RecordReader::Inhouse, root.path());
    assert_eq!(rows(&legacy), rows(&inhouse));

    let conn = inhouse;
    let cell = |column: &str| -> Option<i64> {
        get(
            &conn,
            &format!("SELECT {column} FROM cell_environment WHERE cell_id={LIT_CELL}"),
        )
    };
    assert_eq!(cell("data_flags"), Some(0x81));
    assert_eq!(cell("image_space_id"), Some(IMAGE_SPACE.into()));
    assert_eq!(cell("acoustic_space_id"), Some(ACOUSTICS.into()));
    assert_eq!(cell("music_id"), Some(MUSIC.into()));
    assert_eq!(cell("sky_region_id"), Some(REGION.into()));
    assert_eq!(cell("location_id"), Some(LOCATION.into()));
    assert_eq!(cell("encounter_zone_id"), Some(ZONE.into()));
    assert_eq!(cell("owner_id"), Some(FACTION.into()));
    assert_eq!(cell("owner_rank"), Some(2));
    assert_eq!(cell("water_id"), Some(WATER.into()));
    let regions: i64 = get(&conn, "SELECT count(*) FROM cell_regions");
    assert_eq!(regions, 1, "the region that does not exist is dropped");

    let lit = |column: &str| -> Option<f64> {
        get(
            &conn,
            &format!("SELECT {column} FROM cell_lighting WHERE cell_id={LIT_CELL}"),
        )
    };
    assert_eq!(lit("lighting_template_id"), Some(TEMPLATE.into()));
    assert_eq!(lit("xcll_size"), Some(92.0));
    assert_eq!(lit("ambient_g"), Some(11.0));
    assert_eq!(lit("fog_near_b"), Some(32.0));
    assert_eq!(lit("fog_far"), Some(4096.0));
    assert_eq!(lit("directional_rotation_z"), Some(-30.0));
    assert_eq!(lit("fog_power"), Some(1.5));
    assert_eq!(lit("dalc_x_pos_r"), Some(100.0));
    assert_eq!(lit("dalc_z_neg_b"), Some(125.0));
    assert_eq!(lit("dalc_specular_g"), Some(116.0));
    assert_eq!(lit("dalc_scale"), Some(2.0));
    assert_eq!(lit("fog_far_r"), Some(40.0));
    assert_eq!(lit("light_fade_end"), Some(4000.0));
    assert_eq!(lit("inherit_flags"), Some(f64::from(0x401)));

    let short = |column: &str| -> Option<f64> {
        get(
            &conn,
            &format!("SELECT {column} FROM cell_lighting WHERE cell_id={SHORT_CELL}"),
        )
    };
    assert_eq!(
        short("lighting_template_id"),
        None,
        "an IMGS is not a template"
    );
    assert_eq!(short("dalc_scale"), Some(2.0));
    assert_eq!(short("fog_far_r"), None, "past the 72-byte form");
    assert_eq!(short("inherit_flags"), None);

    let world = |column: &str| -> Option<f64> {
        get(
            &conn,
            &format!("SELECT {column} FROM worldspace_environment WHERE worldspace_id={WORLD}"),
        )
    };
    assert_eq!(world("climate_id"), Some(CLIMATE.into()));
    assert_eq!(
        world("music_id"),
        Some(MOD_MUSIC.into()),
        "ZNAM is remapped through the mod's master list"
    );
    assert_eq!(
        world("lighting_template_id"),
        None,
        "the mod's override has none"
    );

    let template = |column: &str| -> Option<f64> {
        get(
            &conn,
            &format!("SELECT {column} FROM lighting_templates WHERE id={TEMPLATE}"),
        )
    };
    assert_eq!(template("data_size"), Some(92.0));
    assert_eq!(template("fog_far"), Some(4096.0));
    assert_eq!(template("dalc_y_pos_g"), Some(112.0), "DATA's own block");
    assert_eq!(
        template("subrecord_dalc_x_pos_r"),
        Some(170.0),
        "the DALC subrecord"
    );
    assert_eq!(template("subrecord_dalc_scale"), Some(171.0));

    let weather = |id: u32, column: &str| -> Option<f64> {
        get(
            &conn,
            &format!("SELECT {column} FROM weathers WHERE id={id}"),
        )
    };
    assert_eq!(weather(WEATHER, "fog_day_far"), Some(200_000.0));
    assert_eq!(weather(WEATHER, "fog_night_max"), Some(0.5));
    assert_eq!(weather(WEATHER, "wind_speed"), Some(30.0));
    assert_eq!(weather(WEATHER, "classification_flags"), Some(2.0));
    assert_eq!(weather(WEATHER, "lightning_b"), Some(220.0));
    assert_eq!(weather(WEATHER, "disabled_cloud_layers"), Some(5.0));
    assert_eq!(
        weather(WEATHER, "precipitation_id"),
        Some(PRECIPITATION.into())
    );
    assert_eq!(weather(WEATHER, "sky_static_id"), Some(SKY_STATIC.into()));
    assert_eq!(
        weather(WEATHER, "image_space_day_id"),
        Some(IMAGE_SPACE.into())
    );
    assert_eq!(
        weather(WEATHER, "image_space_night_id"),
        None,
        "a null slot"
    );
    assert_eq!(
        weather(MOD_WEATHER, "image_space_sunrise_id"),
        Some(MOD_IMAGE_SPACE.into()),
        "IMSP is remapped through the mod's master list"
    );
    let colour: (i64, i64, i64) = conn
        .query_row(
            &format!(
                "SELECT r, g, b FROM weather_colors WHERE weather_id={WEATHER} AND color_type='unknown' AND time_of_day='sunset'"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(colour, (22, 102, 198), "the third colour type, at sunset");
    let colours: i64 = get(
        &conn,
        &format!("SELECT count(*) FROM weather_colors WHERE weather_id={WEATHER}"),
    );
    assert_eq!(colours, 56, "14 colour types at four times");
    let night_scale: Option<f64> = get(
        &conn,
        &format!(
            "SELECT scale FROM weather_directional_ambient WHERE weather_id={WEATHER} AND time_of_day='night'"
        ),
    );
    assert_eq!(night_scale, Some(131.0), "the fourth DALC is night");

    let climate: Vec<(Option<i64>, i64, Option<i64>)> = conn
        .prepare(&format!(
            "SELECT weather_id, chance, chance_global_id FROM climate_weathers WHERE climate_id={CLIMATE} ORDER BY position"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        climate,
        vec![
            (Some(MOD_WEATHER.into()), -1, Some(GLOBAL.into())),
            (
                Some(WEATHER.into()),
                i64::from(i32::MIN),
                Some(GLOBAL.into())
            ),
            (Some(RAIN.into()), 100, Some(GLOBAL.into())),
        ],
        "the mod's override wins, its WLST links are remapped and signed chances retained"
    );
    let region: Vec<(Option<i64>, i64, Option<i64>)> = conn
        .prepare(
            "SELECT weather_id, chance, chance_global_id FROM region_weathers WHERE region_id=?1 ORDER BY position",
        )
        .unwrap()
        .query_map([REGION], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        region,
        vec![
            (Some(RAIN.into()), 100, None),
            (Some(WEATHER.into()), 0x8000_0000, Some(GLOBAL.into())),
            (Some(RAIN.into()), i64::from(u32::MAX), None),
        ],
        "REGN keeps unsigned high-bit chances nonnegative; 0xBEEF is no global"
    );

    let saturation: Option<f64> = get(
        &conn,
        &format!("SELECT saturation FROM image_spaces WHERE id={MOD_IMAGE_SPACE}"),
    );
    assert_eq!(saturation, Some(0.5));
    let blur: Option<i64> = get(
        &conn,
        &format!("SELECT dof_sky_blur_radius FROM image_spaces WHERE id={IMAGE_SPACE}"),
    );
    assert_eq!(blur, Some(16672));
}
