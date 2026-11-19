//! Convert the same PEX fixtures loaded by Skyrim and record the shipped helper's trace.
use converter::script::ScriptConverter;
use mlua::{Function, Lua, Table, ThreadStatus, Value};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const RUNTIME: &str = include_str!("../../shared/src/papyrus_runtime.luau");
const CLASSES: &[&str] = &[
    "MCParityGrand",
    "MCParityParent",
    "MCParityInheritance",
    "MCParityNestedEnd",
    "MCParityNestedBegin",
    "MCParityLatent",
];

fn snapshot_pex(source: &Path) -> Result<(tempfile::TempDir, BTreeMap<String, String>)> {
    let snapshot = tempfile::tempdir()?;
    let mut hashes = BTreeMap::new();
    for name in CLASSES {
        let filename = format!("{name}.pex");
        let bytes = fs::read(source.join(&filename))?;
        hashes.insert(filename.clone(), format!("{:x}", Sha256::digest(&bytes)));
        fs::write(snapshot.path().join(filename), bytes)?;
    }
    Ok((snapshot, hashes))
}

fn run_case(pex: &Path, class_name: &str, wait_adapter: bool) -> Result<Json> {
    let lua = Lua::new();
    lua.set_memory_limit(32 * 1024 * 1024)?;
    let budget = Arc::new(std::sync::atomic::AtomicU64::new(100_000));
    lua.set_interrupt(move |_| {
        if budget.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            return Err(mlua::Error::RuntimeError(
                "probe instruction budget exhausted".into(),
            ));
        }
        Ok(mlua::VmState::Continue)
    });
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let capture = lines.clone();
    lua.globals().set(
        "record",
        lua.create_function(move |_, line: String| {
            capture.lock().unwrap().push(line);
            Ok(())
        })?,
    )?;
    let module: Table = lua.load(RUNTIME).eval()?;
    let runtime: Table = module.get::<Function>("new")?.call(())?;
    lua.globals().set("rt", runtime.clone())?;
    lua.load(
        r#"
        rt:register_class('Quest', {__name = 'Quest'})
        rt:register_class('Debug', {__name = 'Debug', Trace = function(self, text, severity)
            record(text)
        end})
        rt:register_class('Utility', {__name = 'Utility', Wait = function(self, seconds)
            return self.__runtime:native(self, 'Wait', {seconds})
        end})
    "#,
    )
    .exec()?;
    if wait_adapter {
        lua.load(
            r#"
            -- Probe-only scheduler adapter. This is not shipped engine functionality.
            rt:register_class('Utility', {__name = 'Utility', Wait = function(self, seconds)
                return coroutine.yield({kind = 'Wait', seconds = seconds})
            end})
        "#,
        )
        .exec()?;
    }
    let converted = pex.join("luau");
    fs::create_dir_all(&converted)?;
    let mut digests = serde_json::Map::new();
    for name in CLASSES {
        let input = pex.join(format!("{name}.pex"));
        let output = converted.join(format!("{name}.luau"));
        ScriptConverter::convert_pex_to_luau(&input, &output)?;
        let source = fs::read_to_string(&output)?;
        digests.insert(
            (*name).into(),
            json!(format!("{:x}", Sha256::digest(source.as_bytes()))),
        );
        let class: Table = lua.load(source).set_name(*name).eval()?;
        let register: Function = runtime.get("register_class")?;
        register.call::<()>((runtime.clone(), *name, class))?;
    }
    let class: Table = runtime
        .get::<Table>("classes")?
        .get(class_name.to_lowercase())?;
    let instance: Table = class.get::<Function>("new")?.call(runtime)?;
    let entry: Function = lua
        .load("return function(obj) rt:call_method(obj, 'OnInit', {}) end")
        .eval()?;
    let thread = lua.create_thread(entry)?;
    let first = thread.resume::<Value>(instance);
    let mut yields = Vec::new();
    let mut result = first;
    for _ in 0..8 {
        if result.is_err() || thread.status() != ThreadStatus::Resumable {
            break;
        }
        let token = match result.as_ref().unwrap() {
            Value::Table(token) => token,
            _ => return Err("unexpected probe yield".into()),
        };
        let kind: String = token.get("kind")?;
        let seconds: f64 = token.get("seconds")?;
        if kind != "Wait" || !wait_adapter {
            return Err("unexpected latent adapter invocation".into());
        }
        yields.push(json!({"kind": kind, "seconds": seconds, "trace_before_resume": lines.lock().unwrap().clone()}));
        result = thread.resume::<Value>(());
    }
    let error = result.err().map(|error| error.to_string());
    let trace = lines.lock().unwrap().clone();
    Ok(json!({"trace": trace, "error": error, "yields": yields,
        "finished": thread.status() == ThreadStatus::Finished, "converted_sha256": digests}))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: papyrus_parity PEX_DIRECTORY OUTPUT_JSON".into());
    }
    let (snapshot, pex_hashes) = snapshot_pex(Path::new(&args[0]))?;
    let pex = snapshot.path();
    let mut cases = serde_json::Map::new();
    for (name, class) in [
        ("inheritance", "MCParityInheritance"),
        ("nested_end", "MCParityNestedEnd"),
        ("nested_begin", "MCParityNestedBegin"),
        ("latent", "MCParityLatent"),
    ] {
        cases.insert(name.into(), run_case(pex, class, false)?);
    }
    let adapter = run_case(pex, "MCParityLatent", true)?;
    let result = json!({"scope": "Current converter and shared helper; native services are explicit probe stubs",
        "helper_sha256": format!("{:x}", Sha256::digest(RUNTIME.as_bytes())),
        "pex_sha256": pex_hashes,
        "cases": cases, "probe_only_wait_adapter": adapter});
    let json = serde_json::to_string_pretty(&result)?;
    writeln!(fs::File::create_new(&args[1])?, "{json}")?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_source_bytecode_does_not_change_probe_inputs_or_hashes() -> Result<()> {
        let source = tempfile::tempdir()?;
        for name in CLASSES {
            fs::write(source.path().join(format!("{name}.pex")), b"original")?;
        }
        let (snapshot, hashes) = snapshot_pex(source.path())?;
        for name in CLASSES {
            let filename = format!("{name}.pex");
            fs::write(source.path().join(&filename), b"replacement")?;
            let bytes = fs::read(snapshot.path().join(&filename))?;
            assert_eq!(bytes, b"original");
            assert_eq!(hashes[&filename], format!("{:x}", Sha256::digest(&bytes)));
        }
        Ok(())
    }
}
