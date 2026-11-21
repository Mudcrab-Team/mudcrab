//! The engine's host for converted Papyrus scripts.
//!
//! [`PapyrusHost`] owns the sandboxed Luau VM, loads converted scripts on demand, attaches
//! them to placed references, dispatches events and binds the first slice of natives to a
//! small in-memory [`WorldState`]. It has no Bevy types, so tests drive it with a fake form
//! source and assert on state and [`Effect`]s. `crate::papyrus_plugin` applies the effects to
//! the real scene.
//!
//! Policy (see #630, #575, #645):
//! - A script fault logs and the script continues or ends; it never reaches the engine.
//! - A native nobody bound is counted and logged with its `Class::Function`, and the call
//!   gives `None`. [`PapyrusHost::unbound_natives`] ranks them by call count.
//! - Every entry into script code gets a fresh instruction budget.

use crate::papyrus_runtime::{
    self as runtime, DEFAULT_CALL_BUDGET, DEFAULT_FRAME_BUDGET, DEFAULT_MEMORY_LIMIT,
    ExecutionBudget,
};
use crate::world::enable_state::INITIALLY_DISABLED;
use mlua::{Function, IntoLua, Lua, MultiValue, Table, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// VMAD property value types as the converter writes them to `script_properties` (the
/// decoder is `crates/converter/src/records/vmad.rs`, `scalar` and `property`). An array of a
/// scalar type has the scalar's code plus 10.
const VMAD_OBJECT: i64 = 1;
const VMAD_STRING: i64 = 2;
const VMAD_INT: i64 = 3;
const VMAD_FLOAT: i64 = 4;
const VMAD_BOOL: i64 = 5;
const VMAD_ARRAY_OFFSET: i64 = 10;
const VMAD_ARRAY_FIRST: i64 = VMAD_OBJECT + VMAD_ARRAY_OFFSET;
const VMAD_ARRAY_LAST: i64 = VMAD_BOOL + VMAD_ARRAY_OFFSET;

/// The form id of the player reference.
pub const PLAYER_REFERENCE: u32 = 0x14;

/// Scripts the engine attaches by default. Each is exercised by a headless test in this
/// module and only calls natives the host binds. Every other converted script stays
/// detached until its natives exist, so a script cannot reach an unbound native in play.
pub const SLICE_SCRIPTS: &[&str] = &[
    "defaultDisableSelfOnActivate",
    "defaultUnlockOnLoad",
    "dunDeadMensRotatingDoor",
    "dunPOIDragonNoteScript",
];

/// Something a script asked the engine to do that the engine must apply to the scene.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Disable { reference: u32 },
    Enable { reference: u32 },
    Lock { reference: u32, locked: bool },
    PlayAnimation { reference: u32, event: String },
    AddToMap { reference: u32 },
}

/// Per-quest state the stage natives read and write.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QuestState {
    pub running: bool,
    pub stage: i64,
    pub stages_done: BTreeSet<i64>,
}

/// Everything the natives read and write. Public so tests can seed and inspect it.
#[derive(Debug, Default)]
pub struct WorldState {
    /// References a script disabled, and references a script enabled although they start disabled.
    pub disabled: HashSet<u32>,
    pub enabled: HashSet<u32>,
    pub locked: HashMap<u32, bool>,
    /// Item count per (container reference, item form).
    pub items: HashMap<(u32, u32), i64>,
    /// Faction rank per (actor reference, faction form).
    pub factions: HashMap<(u32, u32), i64>,
    pub quests: HashMap<u32, QuestState>,
    pub map_markers: BTreeSet<u32>,
    /// Reference bound to each (quest, alias id).
    pub aliases: HashMap<(u32, u16), u32>,
    pub effects: Vec<Effect>,
}

/// A log line from the VM or the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub level: String,
    pub message: String,
}

/// One script property as the world database stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptProperty {
    pub name: String,
    pub value: PropertyValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    None,
    Object { form_id: u32, alias: Option<u16> },
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<PropertyValue>),
}

/// Read access to the converted world: which scripts a form carries and what the natives
/// need to know about other forms. The engine uses [`SqliteForms`]; tests use a fake.
pub trait FormSource {
    /// Names of the scripts attached to this form (a reference or a base record).
    fn script_names(&self, form_id: u32) -> Vec<String>;
    fn script_properties(&self, form_id: u32, script_name: &str) -> Vec<ScriptProperty>;
    /// The base record of a placed reference.
    fn base_form(&self, reference: u32) -> Option<u32>;
    fn record_type(&self, form_id: u32) -> Option<String>;
    /// The reference this one links to under `keyword` (`None` = the unkeyed link).
    fn linked_ref(&self, reference: u32, keyword: Option<u32>) -> Option<u32>;
    /// Whether the reference starts enabled. Enable-parent chains are not followed here.
    fn initially_enabled(&self, reference: u32) -> bool;
}

/// Host settings.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Directory holding `<lowercase script name>.luau`.
    pub scripts_dir: PathBuf,
    /// When set, only these scripts (case-insensitive) attach to references.
    pub allowed: Option<HashSet<String>>,
    pub memory_limit: usize,
    pub call_budget: u64,
    /// Interrupts all entries in one frame may use together; see [`ExecutionBudget`].
    pub frame_budget: u64,
    /// Make an unbound native raise instead of log. Tests use this.
    pub strict_natives: bool,
}

impl HostConfig {
    pub fn new(scripts_dir: impl Into<PathBuf>) -> Self {
        Self {
            scripts_dir: scripts_dir.into(),
            allowed: None,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            call_budget: DEFAULT_CALL_BUDGET,
            frame_budget: DEFAULT_FRAME_BUDGET,
            strict_natives: false,
        }
    }

    pub fn allow_only(mut self, scripts: &[&str]) -> Self {
        self.allowed = Some(scripts.iter().map(|name| name.to_lowercase()).collect());
        self
    }
}

/// What attaching scripts to one reference did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AttachReport {
    pub attached: Vec<String>,
    /// Scripts that exist for the form but did not attach, with the reason.
    pub skipped: Vec<(String, String)>,
    /// `false` when the reference already had its scripts and only got cell events again.
    pub first_attach: bool,
}

pub struct PapyrusHost {
    lua: Lua,
    runtime: Table,
    budget: ExecutionBudget,
    world: Rc<RefCell<WorldState>>,
    forms: Rc<dyn FormSource>,
    log: Rc<RefCell<Vec<LogLine>>>,
    allowed: Option<HashSet<String>>,
    attached: HashSet<u32>,
}

impl PapyrusHost {
    pub fn new(config: HostConfig, forms: Rc<dyn FormSource>) -> mlua::Result<Self> {
        let (lua, budget) = runtime::new_sandboxed_vm_with_frame(
            config.memory_limit,
            config.call_budget,
            config.frame_budget,
        )?;
        let runtime_table = runtime::load_runtime(&lua)?;
        runtime_table.set("strict_natives", config.strict_natives)?;
        let log = Rc::new(RefCell::new(Vec::<LogLine>::new()));
        let world = Rc::new(RefCell::new(WorldState::default()));

        let sink = log.clone();
        let logger = lua.create_function(move |_, (level, message): (String, String)| {
            match level.as_str() {
                "error" => bevy::log::error!(target: "papyrus", "{message}"),
                "warn" => bevy::log::warn!(target: "papyrus", "{message}"),
                _ => bevy::log::info!(target: "papyrus", "{message}"),
            }
            let mut lines = sink.borrow_mut();
            if lines.len() >= 1024 {
                lines.remove(0);
            }
            lines.push(LogLine { level, message });
            Ok(())
        })?;
        runtime_table.set("logger", logger)?;

        let reset = budget.clone();
        let reset_fn = lua.create_function(move |_, ()| {
            reset.reset();
            Ok(())
        })?;
        runtime_table.set("reset_budget", reset_fn)?;

        let dir = config.scripts_dir.clone();
        let loader_log = log.clone();
        let loader = lua.create_function(move |lua, lowered: String| -> mlua::Result<Value> {
            if lowered.is_empty() || !lowered.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Ok(Value::Nil);
            }
            let path = dir.join(format!("{lowered}.luau"));
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Value::Nil);
                }
                Err(error) => {
                    loader_log.borrow_mut().push(LogLine {
                        level: "error".into(),
                        message: format!("cannot read script {}: {error}", path.display()),
                    });
                    return Ok(Value::Nil);
                }
            };
            match lua
                .load(source)
                .set_name(format!("@{lowered}.luau"))
                .eval::<Table>()
            {
                Ok(class) => Ok(Value::Table(class)),
                Err(error) => {
                    bevy::log::error!(target: "papyrus", "script {lowered} failed to load: {error}");
                    loader_log.borrow_mut().push(LogLine {
                        level: "error".into(),
                        message: format!("script {lowered} failed to load: {error}"),
                    });
                    Ok(Value::Nil)
                }
            }
        })?;
        runtime_table.set("loader", loader)?;

        bind_natives(&lua, &runtime_table, world.clone(), forms.clone())?;
        Ok(Self {
            lua,
            runtime: runtime_table,
            budget,
            world,
            forms,
            log,
            allowed: config.allowed,
            attached: HashSet::new(),
        })
    }

    pub fn world(&self) -> std::cell::RefMut<'_, WorldState> {
        self.world.borrow_mut()
    }

    /// Make the per-frame script budget whole again. Call once at the start of each frame.
    pub fn begin_frame(&self) {
        self.budget.begin_frame();
    }

    pub fn budget(&self) -> &ExecutionBudget {
        &self.budget
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    pub fn runtime(&self) -> &Table {
        &self.runtime
    }

    /// Everything logged so far (capped at 1,024 lines).
    pub fn log_lines(&self) -> Vec<LogLine> {
        self.log.borrow().clone()
    }

    /// Take the effects scripts queued since the last call.
    pub fn drain_effects(&self) -> Vec<Effect> {
        std::mem::take(&mut self.world.borrow_mut().effects)
    }

    /// Natives scripts called that nothing bound, most-called first.
    pub fn unbound_natives(&self) -> Vec<(String, u64)> {
        let mut rows = Vec::new();
        if let Ok(table) = self.runtime.get::<Table>("unbound") {
            for pair in table.pairs::<String, u64>().flatten() {
                rows.push(pair);
            }
        }
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows
    }

    fn allowed(&self, script_name: &str) -> bool {
        self.allowed
            .as_ref()
            .is_none_or(|set| set.contains(&script_name.to_lowercase()))
    }

    /// Attach the scripts of a placed reference and its base record, seed their
    /// properties, then send `OnInit`, `OnCellAttach` and `OnLoad`. Calling it again for the
    /// same reference only repeats the two cell events.
    pub fn attach_reference(&mut self, reference: u32) -> AttachReport {
        // The base record's values apply first and the reference's values override them.
        let mut owners = Vec::new();
        if let Some(base) = self.forms.base_form(reference) {
            owners.push(base);
        }
        owners.push(reference);
        self.attach_form(
            reference,
            &owners,
            "ObjectReference",
            &["OnInit", "OnCellAttach", "OnLoad"],
        )
    }

    /// Attach the scripts of a quest record and send `OnInit`.
    pub fn attach_quest(&mut self, quest: u32) -> AttachReport {
        self.attach_form(quest, &[quest], "Quest", &["OnInit"])
    }

    fn attach_form(
        &mut self,
        form_id: u32,
        owners: &[u32],
        required_class: &str,
        events: &[&str],
    ) -> AttachReport {
        self.budget.reset();
        let mut report = AttachReport::default();
        if self.attached.contains(&form_id) {
            for event in events.iter().filter(|event| **event != "OnInit") {
                self.send_to_form(form_id, event, &[]);
            }
            return report;
        }
        let mut order: Vec<String> = Vec::new();
        let mut properties: HashMap<String, Vec<ScriptProperty>> = HashMap::new();
        for owner in owners {
            for name in self.forms.script_names(*owner) {
                if !self.allowed(&name) {
                    continue;
                }
                let key = name.to_lowercase();
                if !properties.contains_key(&key) {
                    order.push(name.clone());
                }
                properties
                    .entry(key)
                    .or_default()
                    .extend(self.forms.script_properties(*owner, &name));
            }
        }
        if order.is_empty() {
            return report;
        }
        report.first_attach = true;
        self.attached.insert(form_id);
        let instantiate: Function = match self.runtime.get("instantiate") {
            Ok(function) => function,
            Err(_) => return report,
        };
        for name in order {
            let object: Value =
                match instantiate.call((self.runtime.clone(), name.clone(), form_id)) {
                    Ok(object) => object,
                    Err(error) => {
                        report.skipped.push((name, error.to_string()));
                        continue;
                    }
                };
            let Value::Table(object) = object else {
                report
                    .skipped
                    .push((name, "no converted script with this name".into()));
                continue;
            };
            // silent-ok: a script without __name fails the inherits check below, which reports it.
            let class_name: String = object.get("__name").unwrap_or_default();
            // A script of another base type (a quest script in a reference's VMAD) must not
            // attach (#577): it would run events against the wrong kind of form.
            let inherits: bool = self
                .runtime
                .get::<Function>("inherits")
                .and_then(|f| f.call((self.runtime.clone(), class_name, required_class)))
                .unwrap_or(false);
            if !inherits {
                self.detach(form_id, &object);
                let reason = format!("script does not extend {required_class}");
                self.note(
                    "error",
                    format!("script {name} not attached to {form_id:08X}: {reason}"),
                );
                report.skipped.push((name, reason));
                continue;
            }
            let props = properties
                .get(&name.to_lowercase())
                .cloned()
                .unwrap_or_default(); // silent-ok: no rows means the script declares no properties.
            self.seed_properties(&object, form_id, &name, &props);
            report.attached.push(name);
        }
        for event in events {
            self.send_to_form(form_id, event, &[]);
        }
        report
    }

    /// Remove a rejected script object from the form's list.
    fn detach(&self, reference: u32, object: &Table) {
        if let Ok(objects) = self.runtime.get::<Table>("objects")
            && let Ok(list) = objects.get::<Table>(reference)
        {
            let mut kept = Vec::new();
            for value in list.sequence_values::<Table>().flatten() {
                if !value.equals(object).unwrap_or(false) {
                    kept.push(value);
                }
            }
            if let Ok(fresh) = self.lua.create_sequence_from(kept) {
                let _ = objects.set(reference, fresh);
            }
        }
    }

    fn note(&self, level: &str, message: String) {
        if let Ok(log) = self.runtime.get::<Function>("log") {
            let _ = log.call::<()>((self.runtime.clone(), level, message));
        }
    }

    /// Like [`Self::note`], but each distinct message logs once and the repeats are counted.
    fn note_once(&self, level: &str, message: String) {
        if let Ok(log) = self.runtime.get::<Function>("log_once") {
            let _ = log.call::<()>((self.runtime.clone(), level, message));
        }
    }

    fn seed_properties(
        &self,
        object: &Table,
        reference: u32,
        script: &str,
        props: &[ScriptProperty],
    ) {
        let set: Function = match self.runtime.get("set_property") {
            Ok(function) => function,
            Err(_) => return,
        };
        let find: Function = match self.runtime.get("find_method") {
            Ok(function) => function,
            Err(_) => return,
        };
        for property in props {
            let setter: Value = find
                .call((
                    self.runtime.clone(),
                    object.clone(),
                    "",
                    format!("set_{}", property.name),
                ))
                .unwrap_or(Value::Nil);
            if matches!(setter, Value::Nil) {
                self.note(
                    "warn",
                    format!(
                        "script {script} on {reference:08X} has no property {}; the attached value is ignored",
                        property.name
                    ),
                );
                continue;
            }
            match self.property_value(&property.value, script, &property.name) {
                Ok(value) => {
                    if let Err(error) = set.call::<()>((
                        self.runtime.clone(),
                        object.clone(),
                        property.name.clone(),
                        value,
                    )) {
                        self.note(
                            "error",
                            format!(
                                "script {script}: setting property {} failed: {error}",
                                property.name
                            ),
                        );
                    }
                }
                Err(error) => self.note(
                    "error",
                    format!(
                        "script {script}: property {} could not be built: {error}",
                        property.name
                    ),
                ),
            }
        }
    }

    fn property_value(
        &self,
        value: &PropertyValue,
        script: &str,
        name: &str,
    ) -> mlua::Result<Value> {
        Ok(match value {
            PropertyValue::None => Value::Nil,
            PropertyValue::Int(value) => value.into_lua(&self.lua)?,
            PropertyValue::Float(value) => Value::Number(*value),
            PropertyValue::Bool(value) => Value::Boolean(*value),
            PropertyValue::Str(value) => self.lua.create_string(value)?.into_lua(&self.lua)?,
            PropertyValue::Object { form_id, alias } => {
                if alias.is_some_and(|alias| alias != u16::MAX) {
                    self.note(
                        "warn",
                        format!(
                            "script {script} property {name}: alias-bound objects are not supported yet; the property stays None"
                        ),
                    );
                    return Ok(Value::Nil);
                }
                if *form_id == 0 {
                    return Ok(Value::Nil);
                }
                match self.forms.record_type(*form_id) {
                    Some(record) => {
                        let proxy: Function = self.runtime.get("proxy")?;
                        proxy.call((self.runtime.clone(), *form_id, class_for_record(&record)))?
                    }
                    None => {
                        self.note(
                            "warn",
                            format!(
                                "script {script} property {name}: form {form_id:08X} is not in the world database; the property stays None"
                            ),
                        );
                        Value::Nil
                    }
                }
            }
            PropertyValue::Array(items) => {
                let table = self.lua.create_table()?;
                for (index, item) in items.iter().enumerate() {
                    table.set(index + 1, self.property_value(item, script, name)?)?;
                }
                Value::Table(table)
            }
        })
    }

    /// Send an event to every script object attached to the form. Returns how many objects
    /// had a handler. `args` are form ids turned into reference proxies.
    pub fn send_to_form(&self, form_id: u32, event: &str, args: &[EventArg]) -> usize {
        let Ok(objects) = self.runtime.get::<Table>("objects") else {
            return 0;
        };
        let Ok(list) = objects.get::<Table>(form_id) else {
            return 0;
        };
        let Ok(send): mlua::Result<Function> = self.runtime.get("send_event") else {
            return 0;
        };
        // Building arguments runs Lua (`proxy`), so the budget must be whole first.
        self.budget.reset();
        let mut lua_args = Vec::new();
        for arg in args {
            match arg.to_lua(self) {
                Ok(value) => lua_args.push(value),
                Err(error) => {
                    self.note_once(
                        "error",
                        format!("event {event}: argument could not be built: {error}"),
                    );
                    return 0;
                }
            }
        }
        let mut handled = 0;
        let objects: Vec<Table> = list.sequence_values::<Table>().flatten().collect();
        for object in objects {
            self.budget.reset();
            let mut call_args = MultiValue::new();
            call_args.push_back(Value::Table(self.runtime.clone()));
            call_args.push_back(Value::Table(object));
            // `create_string` fails when the VM is at its memory limit. That is a script
            // problem, never an engine panic: log it and skip this object.
            match self.lua.create_string(event) {
                Ok(name) => call_args.push_back(Value::String(name)),
                Err(error) => {
                    self.note(
                        "error",
                        format!("event {event} on {form_id:08X} skipped: {error}"),
                    );
                    continue;
                }
            }
            for value in &lua_args {
                call_args.push_back(value.clone());
            }
            match send.call::<bool>(call_args) {
                Ok(true) => handled += 1,
                Ok(false) => {}
                Err(error) => {
                    self.note_once("error", format!("event {event} on {form_id:08X}: {error}"))
                }
            }
        }
        handled
    }

    /// The player activated `reference`.
    pub fn activate(&self, reference: u32, activator: u32) -> usize {
        self.send_to_form(reference, "OnActivate", &[EventArg::Reference(activator)])
    }

    /// Advance script time: wake waits and fire registered updates.
    pub fn advance(&self, seconds: f32) {
        self.budget.reset();
        if let Ok(advance) = self.runtime.get::<Function>("advance")
            && let Err(error) = advance.call::<()>((self.runtime.clone(), f64::from(seconds)))
        {
            self.note("error", format!("scheduler failed: {error}"));
        }
    }

    /// Run `chunk` as a script thread so it may `Utility.Wait`. Tests use this to drive
    /// scripts directly; the engine goes through events.
    pub fn run_thread(&self, label: &str, chunk: &str) -> mlua::Result<()> {
        self.budget.reset();
        let function = self
            .lua
            .load(chunk)
            .set_name(format!("@{label}"))
            .into_function()?;
        let spawn: Function = self.runtime.get("spawn")?;
        spawn.call::<()>((self.runtime.clone(), label.to_string(), function))
    }

    /// The script object for a form's first attached script, if any.
    pub fn object(&self, form_id: u32) -> Option<Table> {
        // silent-ok: a missing script object is the expected "no script attached" answer.
        let objects = self.runtime.get::<Table>("objects").ok()?;
        // silent-ok: a missing script object is the expected "no script attached" answer.
        let list = objects.get::<Table>(form_id).ok()?;
        list.get::<Table>(1).ok() // silent-ok: an absent entry means no script attached.
    }

    pub fn is_attached(&self, reference: u32) -> bool {
        self.attached.contains(&reference)
    }
}

/// An event argument as a form reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventArg {
    Reference(u32),
}

impl EventArg {
    fn to_lua(self, host: &PapyrusHost) -> mlua::Result<Value> {
        match self {
            EventArg::Reference(id) => {
                let class = if id == PLAYER_REFERENCE {
                    "Actor"
                } else {
                    "ObjectReference"
                };
                let proxy: Function = host.runtime.get("proxy")?;
                proxy.call((host.runtime.clone(), id, class))
            }
        }
    }
}

/// The converted class a record type's forms are seen as.
pub fn class_for_record(record_type: &str) -> &'static str {
    match record_type {
        "QUST" => "Quest",
        "REFR" | "PGRE" | "PMIS" | "PHZD" | "PARW" | "PBAR" | "PBEA" | "PCON" | "PFLA" => {
            "ObjectReference"
        }
        "ACHR" => "Actor",
        "ACTI" => "Activator",
        "NPC_" => "ActorBase",
        "FACT" => "Faction",
        "KYWD" => "Keyword",
        "MESG" => "Message",
        "GLOB" => "GlobalVariable",
        "WEAP" => "Weapon",
        "ARMO" => "Armor",
        "MISC" => "MiscObject",
        "DOOR" => "Door",
        "CONT" => "Container",
        "BOOK" => "Book",
        "STAT" => "Static",
        "LCTN" => "Location",
        "CELL" => "Cell",
        _ => "Form",
    }
}

// ---- Natives -------------------------------------------------------------------------

type NativeResult = mlua::Result<Value>;

fn form_of(target: &Table) -> mlua::Result<u32> {
    target
        .get::<Option<u32>>("__form_id")?
        .ok_or_else(|| mlua::Error::runtime("this script object has no form id"))
}

fn form_arg(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Table(table)) => table.get::<Option<u32>>("__form_id").ok().flatten(), // silent-ok: a non-form table argument has no form id.
        _ => None,
    }
}

fn int_arg(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Integer(value)) => Some(i64::from(*value)),
        Some(Value::Number(value)) => Some(*value as i64),
        _ => None,
    }
}

fn bool_arg(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Boolean(value)) => *value,
        Some(Value::Nil) | None => default,
        _ => default,
    }
}

fn string_arg(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.to_string_lossy(),
        _ => String::new(),
    }
}

fn runtime_of(target: &Table) -> mlua::Result<Table> {
    target.get("__runtime")
}

fn proxy(lua: &Lua, runtime: &Table, form_id: u32, class: &str) -> NativeResult {
    let _ = lua;
    let make: Function = runtime.get("proxy")?;
    make.call((runtime.clone(), form_id, class))
}

fn log_once(runtime: &Table, key: &str, message: &str) {
    if let Ok(note) = runtime.get::<Function>("note_once") {
        let _ = note.call::<()>((runtime.clone(), key.to_string(), message.to_string()));
    }
}

fn bind_natives(
    lua: &Lua,
    runtime: &Table,
    world: Rc<RefCell<WorldState>>,
    forms: Rc<dyn FormSource>,
) -> mlua::Result<()> {
    fn native(
        lua: &Lua,
        runtime: &Table,
        class: &str,
        function: &str,
        body: impl Fn(&Lua, &Table, &[Value]) -> NativeResult + 'static,
    ) -> mlua::Result<()> {
        let callback = lua.create_function(move |lua, (target, args): (Table, MultiValue)| {
            let args: Vec<Value> = args.into_iter().collect();
            body(lua, &target, &args)
        })?;
        runtime::register_native(runtime, class, function, callback)
    }

    // Game
    native(lua, runtime, "Game", "GetPlayer", {
        move |lua, target, _| proxy(lua, &runtime_of(target)?, PLAYER_REFERENCE, "Actor")
    })?;

    // ObjectReference
    let state = world.clone();
    let source = forms.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "Disable",
        move |_, target, _| {
            let reference = form_of(target)?;
            let mut world = state.borrow_mut();
            let was_enabled = !world.disabled.contains(&reference)
                && (world.enabled.contains(&reference) || source.initially_enabled(reference));
            world.enabled.remove(&reference);
            world.disabled.insert(reference);
            if was_enabled {
                world.effects.push(Effect::Disable { reference });
            }
            Ok(Value::Nil)
        },
    )?;
    let state = world.clone();
    let source = forms.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "Enable",
        move |_, target, _| {
            let reference = form_of(target)?;
            let mut world = state.borrow_mut();
            let was_disabled = world.disabled.contains(&reference)
                || (!world.enabled.contains(&reference) && !source.initially_enabled(reference));
            world.disabled.remove(&reference);
            world.enabled.insert(reference);
            if was_disabled {
                world.effects.push(Effect::Enable { reference });
            }
            Ok(Value::Nil)
        },
    )?;
    let state = world.clone();
    let source = forms.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "IsDisabled",
        move |_, target, _| {
            let reference = form_of(target)?;
            let world = state.borrow();
            let disabled = world.disabled.contains(&reference)
                || (!world.enabled.contains(&reference) && !source.initially_enabled(reference));
            Ok(Value::Boolean(disabled))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "Lock",
        move |_, target, args| {
            let reference = form_of(target)?;
            let locked = bool_arg(args.first(), true);
            let mut world = state.borrow_mut();
            world.locked.insert(reference, locked);
            world.effects.push(Effect::Lock { reference, locked });
            Ok(Value::Nil)
        },
    )?;
    let source = forms.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "GetLinkedRef",
        move |lua, target, args| {
            let reference = form_of(target)?;
            let keyword = form_arg(args.first());
            match source.linked_ref(reference, keyword) {
                Some(linked) => proxy(lua, &runtime_of(target)?, linked, "ObjectReference"),
                None => Ok(Value::Nil),
            }
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "PlayAnimation",
        move |_, target, args| {
            let reference = form_of(target)?;
            let event = string_arg(args.first());
            let runtime = runtime_of(target)?;
            log_once(
                &runtime,
                "host::PlayAnimation",
                "ObjectReference.PlayAnimation has no animation graph to run yet; it returns false",
            );
            state
                .borrow_mut()
                .effects
                .push(Effect::PlayAnimation { reference, event });
            Ok(Value::Boolean(false))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "AddItem",
        move |_, target, args| {
            let container = form_of(target)?;
            let Some(item) = form_arg(args.first()) else {
                return Err(mlua::Error::runtime("AddItem needs an item form"));
            };
            let count = int_arg(args.get(1)).unwrap_or(1);
            *state
                .borrow_mut()
                .items
                .entry((container, item))
                .or_insert(0) += count;
            Ok(Value::Nil)
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "GetItemCount",
        move |_, target, args| {
            let container = form_of(target)?;
            let Some(item) = form_arg(args.first()) else {
                return Err(mlua::Error::runtime("GetItemCount needs an item form"));
            };
            let count = state
                .borrow()
                .items
                .get(&(container, item))
                .copied()
                .unwrap_or(0);
            Ok(Value::Number(count as f64))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "ObjectReference",
        "AddToMap",
        move |_, target, _| {
            let reference = form_of(target)?;
            let runtime = runtime_of(target)?;
            log_once(
                &runtime,
                "host::AddToMap",
                "ObjectReference.AddToMap records the marker as discovered; there is no map to show it on yet",
            );
            let mut world = state.borrow_mut();
            if world.map_markers.insert(reference) {
                world.effects.push(Effect::AddToMap { reference });
            }
            Ok(Value::Nil)
        },
    )?;

    // Actor
    let state = world.clone();
    let runtime_for_log = runtime.clone();
    native(
        lua,
        runtime,
        "Actor",
        "IsInFaction",
        move |_, target, args| {
            let actor = form_of(target)?;
            let Some(faction) = form_arg(args.first()) else {
                return Ok(Value::Boolean(false));
            };
            log_once(
                &runtime_for_log,
                "host::IsInFaction",
                "Actor.IsInFaction sees only ranks scripts set; base-record faction membership is not read yet",
            );
            let rank = state.borrow().factions.get(&(actor, faction)).copied();
            Ok(Value::Boolean(rank.is_some_and(|rank| rank >= 0)))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "Actor",
        "SetFactionRank",
        move |_, target, args| {
            let actor = form_of(target)?;
            let Some(faction) = form_arg(args.first()) else {
                return Err(mlua::Error::runtime("SetFactionRank needs a faction form"));
            };
            let rank = int_arg(args.get(1)).unwrap_or(0);
            state.borrow_mut().factions.insert((actor, faction), rank);
            Ok(Value::Nil)
        },
    )?;

    // ReferenceAlias
    let state = world.clone();
    native(
        lua,
        runtime,
        "ReferenceAlias",
        "GetReference",
        move |lua, target, _| {
            let quest = form_of(target)?;
            let alias = target.get::<Option<i64>>("__alias_id")?.unwrap_or(-1);
            let bound = u16::try_from(alias) // silent-ok: the -1 default and out-of-range ids match no alias.
                .ok()
                .and_then(|alias| state.borrow().aliases.get(&(quest, alias)).copied());
            match bound {
                Some(reference) => proxy(lua, &runtime_of(target)?, reference, "ObjectReference"),
                None => Ok(Value::Nil),
            }
        },
    )?;

    // Quest
    let state = world.clone();
    native(
        lua,
        runtime,
        "Quest",
        "IsStageDone",
        move |_, target, args| {
            let quest = form_of(target)?;
            let stage = int_arg(args.first()).unwrap_or(0);
            let done = state
                .borrow()
                .quests
                .get(&quest)
                .is_some_and(|quest| quest.stages_done.contains(&stage));
            Ok(Value::Boolean(done))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "Quest",
        "GetCurrentStageID",
        move |_, target, _| {
            let quest = form_of(target)?;
            let stage = state
                .borrow()
                .quests
                .get(&quest)
                .map_or(0, |quest| quest.stage);
            Ok(Value::Number(stage as f64))
        },
    )?;
    let state = world.clone();
    native(
        lua,
        runtime,
        "Quest",
        "SetCurrentStageID",
        move |_, target, args| {
            let quest = form_of(target)?;
            let stage = int_arg(args.first()).unwrap_or(0);
            let mut world = state.borrow_mut();
            let entry = world.quests.entry(quest).or_default();
            entry.stage = stage;
            entry.stages_done.insert(stage);
            Ok(Value::Nil)
        },
    )?;
    let state = world.clone();
    native(lua, runtime, "Quest", "Start", move |_, target, _| {
        let quest = form_of(target)?;
        state.borrow_mut().quests.entry(quest).or_default().running = true;
        Ok(Value::Boolean(true))
    })?;
    let state = world;
    native(lua, runtime, "Quest", "IsRunning", move |_, target, _| {
        let quest = form_of(target)?;
        let running = state
            .borrow()
            .quests
            .get(&quest)
            .is_some_and(|quest| quest.running);
        Ok(Value::Boolean(running))
    })?;
    Ok(())
}

// ---- Sqlite form source --------------------------------------------------------------

/// A [`FormSource`] over the converted pack's `skyrim_world.db`, opened read-only.
pub struct SqliteForms {
    connection: rusqlite::Connection,
    scripted: HashSet<u32>,
}

impl SqliteForms {
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let scripted = {
            let mut statement = connection
                .prepare("SELECT DISTINCT form_id FROM scripts")
                .map_err(|error| format!("{} has no scripts table: {error}", path.display()))?;
            let rows = statement
                .query_map([], |row| row.get::<_, u32>(0))
                .map_err(|error| error.to_string())?;
            rows.flatten().collect()
        };
        // The in-house record reader writes properties to these two tables. The legacy reader
        // stores them as JSON in `scripts.properties_json` instead, which this host does not
        // read. Refuse such a pack: attaching its scripts without their properties would
        // silently run them with every property `None`.
        let property_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
                 AND name IN ('script_properties', 'script_property_values')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if property_tables != 2 {
            return Err(format!(
                "{} has no script_properties and script_property_values tables, so it was made \
                 by the legacy record reader, which stores script properties as JSON. Regenerate \
                 the pack with the in-house reader",
                path.display()
            ));
        }
        Ok(Self {
            connection,
            scripted,
        })
    }

    fn one_u32(&self, sql: &str, id: u32) -> Option<u32> {
        self.connection
            .prepare_cached(sql) // silent-ok: the schema is checked at open; an error means no value.
            .ok()?
            .query_row([id], |row| row.get::<_, Option<u32>>(0)) // silent-ok: an absent row means no value.
            .ok()
            .flatten()
    }
}

impl FormSource for SqliteForms {
    fn script_names(&self, form_id: u32) -> Vec<String> {
        if !self.scripted.contains(&form_id) {
            return Vec::new();
        }
        let Ok(mut statement) = self.connection.prepare_cached(
            "SELECT script_name FROM scripts WHERE form_id = ?1 ORDER BY script_name",
        ) else {
            return Vec::new();
        };
        statement
            .query_map([form_id], |row| row.get::<_, String>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default() // silent-ok: a row that fails to decode is dropped from the list.
    }

    fn script_properties(&self, form_id: u32, script_name: &str) -> Vec<ScriptProperty> {
        let Ok(mut properties) = self.connection.prepare_cached(
            "SELECT property_index, name, property_type FROM script_properties \
             WHERE form_id = ?1 AND script_name = ?2 ORDER BY property_index",
        ) else {
            return Vec::new();
        };
        let Ok(mut values) = self.connection.prepare_cached(
            "SELECT object_form_id, alias_id, string_value, integer_value, float_bits, boolean_value \
             FROM script_property_values \
             WHERE form_id = ?1 AND script_name = ?2 AND property_index = ?3 ORDER BY value_index",
        ) else {
            return Vec::new();
        };
        let rows: Vec<(i64, String, i64)> = properties
            .query_map(rusqlite::params![form_id, script_name], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default(); // silent-ok: a row that fails to decode is dropped from the list.
        let mut output = Vec::new();
        for (index, name, kind) in rows {
            let scalar_kind = if (VMAD_ARRAY_FIRST..=VMAD_ARRAY_LAST).contains(&kind) {
                kind - VMAD_ARRAY_OFFSET
            } else {
                kind
            };
            let elements: Vec<PropertyValue> = values
                .query_map(rusqlite::params![form_id, script_name, index], |row| {
                    let object: Option<u32> = row.get(0)?;
                    let alias: Option<i64> = row.get(1)?;
                    let string: Option<String> = row.get(2)?;
                    let integer: Option<i64> = row.get(3)?;
                    let float_bits: Option<i64> = row.get(4)?;
                    let boolean: Option<i64> = row.get(5)?;
                    Ok(match scalar_kind {
                        VMAD_OBJECT => PropertyValue::Object {
                            form_id: object.unwrap_or(0),
                            alias: alias.and_then(|alias| u16::try_from(alias & 0xFFFF).ok()), // silent-ok: the 16-bit mask makes this infallible.
                        },
                        VMAD_STRING => PropertyValue::Str(string.unwrap_or_default()), // silent-ok: an empty string is the default for an unfilled column.
                        VMAD_INT => PropertyValue::Int(integer.unwrap_or(0)),
                        VMAD_FLOAT => PropertyValue::Float(f64::from(f32::from_bits(
                            float_bits.unwrap_or(0) as u32,
                        ))),
                        VMAD_BOOL => PropertyValue::Bool(boolean.unwrap_or(0) != 0),
                        _ => PropertyValue::None,
                    })
                })
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(); // silent-ok: a row that fails to decode is dropped from the array.
            let value = if (VMAD_ARRAY_FIRST..=VMAD_ARRAY_LAST).contains(&kind) {
                PropertyValue::Array(elements)
            } else {
                elements.into_iter().next().unwrap_or(PropertyValue::None)
            };
            output.push(ScriptProperty { name, value });
        }
        output
    }

    fn base_form(&self, reference: u32) -> Option<u32> {
        self.one_u32(
            "SELECT base_form_id FROM \"references\" WHERE id = ?1",
            reference,
        )
    }

    fn record_type(&self, form_id: u32) -> Option<String> {
        self.connection
            .prepare_cached("SELECT record_type FROM records WHERE form_id = ?1") // silent-ok: an error means no record type.
            .ok()?
            .query_row([form_id], |row| row.get::<_, String>(0)) // silent-ok: an absent row means no record type.
            .ok()
    }

    fn linked_ref(&self, reference: u32, keyword: Option<u32>) -> Option<u32> {
        self.connection
            .prepare_cached(
                "SELECT linked_ref_id FROM reference_linked_refs \
                 WHERE reference_id = ?1 AND keyword_id IS ?2 ORDER BY position LIMIT 1",
            ) // silent-ok: an error means no linked reference.
            .ok()?
            .query_row(rusqlite::params![reference, keyword], |row| {
                row.get::<_, Option<u32>>(0)
            }) // silent-ok: an absent row means no linked reference.
            .ok()
            .flatten()
    }

    fn initially_enabled(&self, reference: u32) -> bool {
        // The "initially disabled" header flag, the one the streamer's enable-state resolver
        // reads. Enable-parent chains are not followed here.
        self.one_u32(
            "SELECT header_flags FROM \"references\" WHERE id = ?1",
            reference,
        )
        .is_none_or(|flags| flags & INITIALLY_DISABLED == 0)
    }
}

/// A map of in-memory forms for tests and tools.
#[derive(Default)]
pub struct MemoryForms {
    pub scripts: BTreeMap<u32, Vec<(String, Vec<ScriptProperty>)>>,
    pub bases: HashMap<u32, u32>,
    pub records: HashMap<u32, String>,
    pub links: HashMap<(u32, Option<u32>), u32>,
    pub initially_disabled: HashSet<u32>,
}

impl FormSource for MemoryForms {
    fn script_names(&self, form_id: u32) -> Vec<String> {
        self.scripts
            .get(&form_id)
            .map(|list| list.iter().map(|(name, _)| name.clone()).collect()) // silent-ok: an unlisted form has no scripts.
            .unwrap_or_default()
    }

    fn script_properties(&self, form_id: u32, script_name: &str) -> Vec<ScriptProperty> {
        self.scripts
            .get(&form_id)
            .and_then(|list| list.iter().find(|(name, _)| name == script_name))
            .map(|(_, props)| props.clone()) // silent-ok: an unlisted form has no properties.
            .unwrap_or_default()
    }

    fn base_form(&self, reference: u32) -> Option<u32> {
        self.bases.get(&reference).copied()
    }

    fn record_type(&self, form_id: u32) -> Option<String> {
        self.records.get(&form_id).cloned()
    }

    fn linked_ref(&self, reference: u32, keyword: Option<u32>) -> Option<u32> {
        self.links.get(&(reference, keyword)).copied()
    }

    fn initially_enabled(&self, reference: u32) -> bool {
        !self.initially_disabled.contains(&reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const REFERENCE: u32 = 0x1000;
    const QUEST: u32 = 0x2000;
    const MARKER: u32 = 0x3000;
    const CONTROL_QUEST: u32 = 0x4000;

    fn class(name: &str, parent: &str, auto_state: &str, body: &str) -> String {
        format!(
            r#"local Script = {{}}
Script.__index = Script
Script.__name = "{name}"
Script.__parent = "{parent}"
Script.__autoState = "{auto_state}"
Script.__types = {{}}
function Script.new(runtime)
    local self = setmetatable({{ __runtime = runtime }}, Script)
    return self
end
{body}
return Script
"#
        )
    }

    /// A converted class whose functions are native wrappers, as the emitter writes them.
    fn natives(name: &str, parent: &str, functions: &[(&str, usize)]) -> String {
        let body: String = functions
            .iter()
            .map(|(function, arity)| {
                let params: Vec<String> = (0..*arity).map(|i| format!("p{i}")).collect();
                format!(
                    "Script[\"{function}\"] = function(self{}) local __rt = self.__runtime return __rt:native(self, \"{function}\", {{{}}}) end\n",
                    params.iter().map(|p| format!(", {p}")).collect::<String>(),
                    params.join(", ")
                )
            })
            .collect();
        class(name, parent, "", &body)
    }

    fn property(name: &str) -> String {
        format!(
            "Script[\"get_{name}\"] = function(self) return self[\"::{name}_var\"] end\n\
             Script[\"set_{name}\"] = function(self, value) self[\"::{name}_var\"] = value end\n"
        )
    }

    /// Hand-written classes in the shape the converter emits, for the engine classes the
    /// slice needs and for each slice script. Real converted scripts are checked by the
    /// `real_pack_slice` test when `MUDCRAB_PAPYRUS_SCRIPTS` names a pack's script folder.
    fn fixture_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let write = |name: &str, text: String| {
            std::fs::write(dir.path().join(format!("{name}.luau")), text).unwrap();
        };
        write("form", natives("Form", "", &[("RegisterForUpdate", 1)]));
        write(
            "objectreference",
            natives(
                "ObjectReference",
                "Form",
                &[
                    ("Disable", 1),
                    ("Enable", 1),
                    ("IsDisabled", 0),
                    ("Lock", 2),
                    ("GetLinkedRef", 1),
                    ("PlayAnimation", 1),
                    ("AddItem", 3),
                    ("GetItemCount", 1),
                    ("AddToMap", 1),
                ],
            ),
        );
        write(
            "actor",
            natives(
                "Actor",
                "ObjectReference",
                &[("IsInFaction", 1), ("SetFactionRank", 2)],
            ),
        );
        write(
            "quest",
            class(
                "Quest",
                "Form",
                "",
                &(natives_body(&[
                    ("Start", 0),
                    ("IsRunning", 0),
                    ("IsStageDone", 1),
                    ("GetCurrentStageID", 0),
                    ("SetCurrentStageID", 1),
                ]) + "Script[\"GetStage\"] = function(self) return self.__runtime:call_method(self, \"GetCurrentStageID\", {}) end\n\
                      Script[\"SetStage\"] = function(self, stage) self.__runtime:call_method(self, \"SetCurrentStageID\", {stage}) return true end\n\
                      Script[\"GetStageDone\"] = function(self, stage) return self.__runtime:call_method(self, \"IsStageDone\", {stage}) end\n"),
            ),
        );
        write("game", natives("Game", "", &[("GetPlayer", 0)]));
        write("utility", natives("Utility", "", &[("Wait", 1)]));
        // Mixed-case parent names and lowercase method names, as in the converted pack.
        write(
            "defaultdisableonreset",
            class(
                "defaultDisableOnReset",
                "objectReference",
                "",
                "Script[\"OnReset\"] = function(self) local __rt = self.__runtime __rt:call_method(self, \"Disable\", {false}) end\n",
            ),
        );
        write(
            "defaultdisableselfonactivate",
            class(
                "defaultDisableSelfOnActivate",
                "ObjectReference",
                "WaitingToBeActivated",
                "Script[\"WaitingToBeActivated::OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                 __rt:call_method(self, \"Disable\", {false}) __rt:call_method(self, \"GotoState\", {\"Disabled\"}) end\n",
            ),
        );
        write(
            "defaultunlockonload",
            class(
                "defaultUnlockOnLoad",
                "ObjectReference",
                "",
                &("Script[\"OnLoad\"] = function(self) local __rt = self.__runtime \
                   local done = __rt:call_method(self[\"::myQuest_var\"], \"GetStageDone\", {self[\"::unlockStage_var\"]}) \
                   if done then __rt:call_method(self, \"Lock\", {false, false}) end end\n"
                    .to_string()
                    + &property("myQuest")
                    + &property("unlockStage")),
            ),
        );
        write(
            "dundeadmensrotatingdoor",
            class(
                "dunDeadMensRotatingDoor",
                "ObjectReference",
                "",
                &("Script[\"onActivate\"] = function(self, p0) local __rt = self.__runtime \
                   if not self[\"::startOpen_var\"] then __rt:call_method(self, \"playAnimation\", {\"RotateClosed\"}) self[\"::startOpen_var\"] = true \
                   else __rt:call_method(self, \"playAnimation\", {\"RotateOpen\"}) self[\"::startOpen_var\"] = false end end\n\
                   Script[\"onLoad\"] = function(self) local __rt = self.__runtime \
                   if not self[\"::startOpen_var\"] then __rt:call_method(self, \"playAnimation\", {\"SnapOpen\"}) \
                   else __rt:call_method(self, \"playAnimation\", {\"SnapClosed\"}) end end\n"
                    .to_string()
                    + &property("startOpen")),
            ),
        );
        write(
            "dunpoidragonnotescript",
            class(
                "dunPOIDragonNoteScript",
                "ObjectReference",
                "",
                &("Script[\"OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                   __rt:call_method(self[\"::mapMarker_var\"], \"addtoMap\", {false}) end\n"
                    .to_string()
                    + &property("mapMarker")),
            ),
        );
        write(
            "dbrecurringcontrolquestscript",
            class(
                "DBRecurringControlQuestScript",
                "Quest",
                "",
                &("Script[\"OnUpdate\"] = function(self) local __rt = self.__runtime \
                   if __rt:call_method(self[\"::DBRecurring_var\"], \"GetStage\", {}) >= 200 then \
                   __rt:call_static(\"utility\", \"Wait\", {2.0}) \
                   __rt:call_method(self[\"::DBRecurring_var\"], \"SetStage\", {10}) end end\n"
                    .to_string()
                    + &property("DBRecurring")),
            ),
        );
        dir
    }

    fn natives_body(functions: &[(&str, usize)]) -> String {
        let text = natives("X", "", functions);
        let start = text.find("Script[\"").unwrap();
        let end = text.rfind("return Script").unwrap();
        text[start..end].to_string()
    }

    fn base_forms() -> MemoryForms {
        let mut forms = MemoryForms::default();
        for (id, record) in [
            (REFERENCE, "REFR"),
            (QUEST, "QUST"),
            (MARKER, "REFR"),
            (CONTROL_QUEST, "QUST"),
            (PLAYER_REFERENCE, "ACHR"),
        ] {
            forms.records.insert(id, record.into());
        }
        forms
    }

    fn host_with(dir: &Path, forms: MemoryForms, strict: bool) -> PapyrusHost {
        let mut config = HostConfig::new(dir);
        config.strict_natives = strict;
        PapyrusHost::new(config, Rc::new(forms)).unwrap()
    }

    fn attach(forms: &mut MemoryForms, form: u32, script: &str, props: Vec<ScriptProperty>) {
        forms
            .scripts
            .entry(form)
            .or_default()
            .push((script.into(), props));
    }

    fn errors(host: &PapyrusHost) -> Vec<String> {
        host.log_lines()
            .into_iter()
            .filter(|line| line.level == "error")
            .map(|line| line.message)
            .collect()
    }

    fn state_of(host: &PapyrusHost, form: u32) -> String {
        let object = host.object(form).unwrap();
        let call: Function = host.runtime().get("call_method").unwrap();
        let args = host.lua().create_table().unwrap();
        call.call((host.runtime().clone(), object, "GetState", args))
            .unwrap()
    }

    fn object_prop(name: &str, form_id: u32) -> ScriptProperty {
        ScriptProperty {
            name: name.into(),
            value: PropertyValue::Object {
                form_id,
                alias: Some(u16::MAX),
            },
        }
    }

    /// The slice assertions, shared by the hand-written fixtures and real converted scripts.
    fn exercise_slice(dir: &Path) {
        // defaultDisableSelfOnActivate: activate hides the reference once, then the Disabled
        // state has no OnActivate handler.
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "defaultDisableSelfOnActivate",
            vec![],
        );
        let mut host = host_with(dir, forms, true);
        let report = host.attach_reference(REFERENCE);
        assert_eq!(
            report.attached,
            vec!["defaultDisableSelfOnActivate"],
            "{report:?}"
        );
        assert_eq!(state_of(&host, REFERENCE), "WaitingToBeActivated");
        assert_eq!(host.activate(REFERENCE, PLAYER_REFERENCE), 1);
        assert_eq!(
            host.drain_effects(),
            vec![Effect::Disable {
                reference: REFERENCE
            }]
        );
        assert!(host.world().disabled.contains(&REFERENCE));
        assert_eq!(state_of(&host, REFERENCE), "Disabled");
        // The Disabled state has no OnActivate of its own. A real parent class declares an
        // empty one, so the handler count is not the check; the absence of effects is.
        host.activate(REFERENCE, PLAYER_REFERENCE);
        assert!(host.drain_effects().is_empty());
        assert_eq!(errors(&host), Vec::<String>::new());

        // defaultDisableOnReset: OnReset disables once.
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "defaultDisableOnReset", vec![]);
        let mut host = host_with(dir, forms, true);
        host.attach_reference(REFERENCE);
        assert!(host.drain_effects().is_empty());
        host.send_to_form(REFERENCE, "OnReset", &[]);
        assert_eq!(
            host.drain_effects(),
            vec![Effect::Disable {
                reference: REFERENCE
            }]
        );
        host.send_to_form(REFERENCE, "OnReset", &[]);
        assert!(
            host.drain_effects().is_empty(),
            "a second Disable changes nothing"
        );

        // defaultUnlockOnLoad: unlocks only when the quest stage is done.
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "defaultUnlockOnLoad",
            vec![
                object_prop("myQuest", QUEST),
                ScriptProperty {
                    name: "unlockStage".into(),
                    value: PropertyValue::Int(10),
                },
            ],
        );
        let mut host = host_with(dir, forms, true);
        host.attach_reference(REFERENCE);
        assert!(host.drain_effects().is_empty(), "stage 10 not done yet");
        host.world()
            .quests
            .entry(QUEST)
            .or_default()
            .stages_done
            .insert(10);
        host.attach_reference(REFERENCE);
        assert_eq!(
            host.drain_effects(),
            vec![Effect::Lock {
                reference: REFERENCE,
                locked: false
            }]
        );
        assert_eq!(host.world().locked.get(&REFERENCE), Some(&false));
        assert_eq!(errors(&host), Vec::<String>::new());

        // dunDeadMensRotatingDoor: load snaps the door, each activation rotates it the other way.
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "dunDeadMensRotatingDoor",
            vec![ScriptProperty {
                name: "startOpen".into(),
                value: PropertyValue::Bool(false),
            }],
        );
        let mut host = host_with(dir, forms, true);
        host.attach_reference(REFERENCE);
        let animation = |event: &str| Effect::PlayAnimation {
            reference: REFERENCE,
            event: event.into(),
        };
        assert_eq!(host.drain_effects(), vec![animation("SnapOpen")]);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        assert_eq!(
            host.drain_effects(),
            vec![animation("RotateClosed"), animation("RotateOpen")]
        );
        assert!(
            host.log_lines().iter().any(|line| line
                .message
                .contains("PlayAnimation has no animation graph")),
            "the partial native says so once"
        );

        // dunPOIDragonNoteScript: activating the note marks the map marker it links to.
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "dunPOIDragonNoteScript",
            vec![object_prop("mapMarker", MARKER)],
        );
        let mut host = host_with(dir, forms, true);
        host.attach_reference(REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        assert_eq!(
            host.drain_effects(),
            vec![Effect::AddToMap { reference: MARKER }]
        );
        assert!(host.world().map_markers.contains(&MARKER));

        // DBRecurringControlQuestScript: OnUpdate waits two seconds, then sets stage 10.
        let mut forms = base_forms();
        attach(
            &mut forms,
            CONTROL_QUEST,
            "DBRecurringControlQuestScript",
            vec![object_prop("DBRecurring", QUEST)],
        );
        let mut host = host_with(dir, forms, true);
        host.attach_quest(CONTROL_QUEST);
        host.world().quests.entry(QUEST).or_default().stage = 200;
        host.send_to_form(CONTROL_QUEST, "OnUpdate", &[]);
        assert_eq!(host.world().quests[&QUEST].stage, 200, "still waiting");
        host.advance(1.0);
        assert_eq!(
            host.world().quests[&QUEST].stage,
            200,
            "one second is not two"
        );
        host.advance(1.0);
        assert_eq!(host.world().quests[&QUEST].stage, 10);
        assert_eq!(errors(&host), Vec::<String>::new());
    }

    #[test]
    fn slice_scripts_run_against_hand_written_fixtures() {
        let dir = fixture_dir();
        exercise_slice(dir.path());
    }

    /// Runs the same assertions on real converted scripts. Set `MUDCRAB_PAPYRUS_SCRIPTS` to a
    /// pack's `scripts` folder; the test passes without running when it is unset.
    #[test]
    fn real_pack_slice() {
        let Some(dir) = std::env::var_os("MUDCRAB_PAPYRUS_SCRIPTS") else {
            return;
        };
        exercise_slice(Path::new(&dir));
    }

    #[test]
    fn inherited_natives_resolve_through_the_parent_chain_and_none_calls_continue() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("faulty.luau"),
            class(
                "Faulty",
                "Actor",
                "",
                "Script[\"OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                 __rt:call_method(nil, \"Disable\", {false}) \
                 __rt:call_method(self, \"NoSuchMethod\", {}) \
                 __rt:call_static(\"nosuchclass\", \"X\", {}) \
                 self.after = __rt:call_method(self[\"::nothing\"], \"GetStage\", {}) == nil \
                 __rt:call_method(self, \"SetFactionRank\", {nil, 1}) end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "faulty", vec![]);
        let mut host = host_with(dir.path(), forms, false);
        host.attach_reference(REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        assert!(
            object.get::<bool>("after").unwrap(),
            "the script ran to the end"
        );
        let log = errors(&host).join("\n");
        for expected in [
            "call to Disable on None",
            "unknown Papyrus method: Faulty::NoSuchMethod",
            "unknown Papyrus class: nosuchclass",
            "call to GetStage on None",
        ] {
            assert!(log.contains(expected), "missing {expected:?} in:\n{log}");
        }
        // SetFactionRank on an Actor script found its native on Actor through the child class.
        assert!(!log.contains("unbound"), "{log}");
    }

    #[test]
    fn an_unbound_native_is_logged_with_its_class_and_counted_never_silent() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("callsunbound.luau"),
            class(
                "CallsUnbound",
                "ObjectReference",
                "",
                "Script[\"OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                 self.result = __rt:call_method(self, \"GetLinkedRefChain\", {}) end\n\
                 Script[\"GetLinkedRefChain\"] = function(self) local __rt = self.__runtime \
                 return __rt:native(self, \"GetLinkedRefChain\", {}) end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "callsunbound", vec![]);
        let mut host = host_with(dir.path(), forms, false);
        host.attach_reference(REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        assert_eq!(
            host.unbound_natives(),
            vec![("CallsUnbound::GetLinkedRefChain".to_string(), 2)]
        );
        let log = errors(&host);
        assert_eq!(
            log.len(),
            1,
            "first call logs, later calls only count: {log:?}"
        );
        assert!(log[0].contains("unbound Papyrus native CallsUnbound::GetLinkedRefChain"));

        // Strict mode, for tests and CI, turns the same call into a script error.
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "callsunbound", vec![]);
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        assert!(
            errors(&host)
                .iter()
                .any(|line| line.contains("unbound Papyrus native"))
        );
    }

    #[test]
    fn a_wait_suspends_one_script_while_updates_and_other_scripts_keep_running() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("waiter.luau"),
            class(
                "Waiter",
                "ObjectReference",
                "",
                "Script[\"OnInit\"] = function(self) self.updates = 0 self.resumed = 0 \
                 self.__runtime:call_method(self, \"RegisterForUpdate\", {0.1}) end\n\
                 Script[\"OnUpdate\"] = function(self) self.updates = self.updates + 1 end\n\
                 Script[\"OnActivate\"] = function(self, p0) local __rt = self.__runtime \
                 __rt:call_static(\"utility\", \"Wait\", {0.5}) self.resumed = self.resumed + 1 \
                 self.resumed_at = __rt.clock end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "waiter", vec![]);
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        let mut seen_during_wait = 0;
        for tick in 1..=30 {
            host.advance(0.033);
            if tick == 14 {
                // 0.462 s: the wait has not finished, the updates have kept firing.
                assert_eq!(object.get::<i64>("resumed").unwrap(), 0);
                seen_during_wait = object.get::<i64>("updates").unwrap();
            }
        }
        assert!(
            seen_during_wait >= 4,
            "updates during the wait: {seen_during_wait}"
        );
        assert_eq!(
            object.get::<i64>("resumed").unwrap(),
            1,
            "the stack resumed exactly once"
        );
        let resumed_at: f64 = object.get("resumed_at").unwrap();
        assert!(
            (0.5..0.5 + 0.034).contains(&resumed_at),
            "resumed at {resumed_at}"
        );
        assert!(
            object.get::<i64>("updates").unwrap() >= 9,
            "about ten updates in a second"
        );
        assert_eq!(errors(&host), Vec::<String>::new());
    }

    #[test]
    fn a_runaway_script_fails_alone_with_its_name_and_the_next_call_runs() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("spinner.luau"),
            class(
                "Spinner",
                "ObjectReference",
                "",
                "Script[\"OnActivate\"] = function(self, p0) while true do end end\n\
                 Script[\"OnReset\"] = function(self) self.ran = true end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "spinner", vec![]);
        let mut config = HostConfig::new(dir.path());
        config.call_budget = 5_000;
        let mut host = PapyrusHost::new(config, Rc::new(forms)).unwrap();
        host.attach_reference(REFERENCE);
        for _ in 0..3 {
            host.activate(REFERENCE, PLAYER_REFERENCE);
            host.send_to_form(REFERENCE, "OnReset", &[]);
            assert!(host.object(REFERENCE).unwrap().get::<bool>("ran").unwrap());
        }
        // Every runaway call failed and none poisoned the VM (`ran` was set each round), but
        // the log carries one line, with the count kept in the runtime.
        let failures: Vec<String> = errors(&host)
            .into_iter()
            .filter(|line| line.contains("budget exceeded"))
            .collect();
        assert_eq!(failures.len(), 1, "logged once: {failures:?}");
        assert!(
            failures[0].starts_with("Spinner::OnActivate:"),
            "{}",
            failures[0]
        );
        let counts: Table = host.runtime().get("fault_counts").unwrap();
        assert_eq!(counts.get::<i64>(failures[0].clone()).unwrap(), 3);
    }

    #[test]
    fn many_events_in_one_frame_share_a_total_and_a_new_frame_refills_it() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("burner.luau"),
            class(
                "Burner",
                "ObjectReference",
                "",
                "Script[\"OnActivate\"] = function(self, p0) local n = 0 for i = 1, 2000 do n = n + i end self.runs = (self.runs or 0) + 1 end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "burner", vec![]);
        // One entry costs roughly a few thousand interrupts; allow each 100,000 but the frame
        // only enough for about three.
        let mut config = HostConfig::new(dir.path());
        config.call_budget = 100_000;
        let mut host = PapyrusHost::new(config.clone(), Rc::new(forms)).unwrap();
        host.attach_reference(REFERENCE);
        host.begin_frame();
        host.budget().reset();
        let before = host.budget().frame_remaining();
        host.activate(REFERENCE, PLAYER_REFERENCE);
        let cost = before - host.budget().frame_remaining();
        // The cost is only known after the next refill charges it.
        host.budget().reset();
        let one = before - host.budget().frame_remaining();
        assert!(one > 0, "an entry costs something ({cost})");

        let mut config = HostConfig::new(dir.path());
        config.call_budget = 100_000;
        config.frame_budget = one * 3;
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "burner", vec![]);
        let mut host = PapyrusHost::new(config, Rc::new(forms)).unwrap();
        host.attach_reference(REFERENCE);
        host.begin_frame();
        for _ in 0..10 {
            host.activate(REFERENCE, PLAYER_REFERENCE);
        }
        let runs = host
            .object(REFERENCE)
            .unwrap()
            .get::<i64>("runs")
            .unwrap_or(0);
        assert!(
            (1..10).contains(&runs),
            "some ran, the rest were cut off: {runs}"
        );
        let cut_off = errors(&host)
            .iter()
            .filter(|line| line.contains("budget exceeded"))
            .count();
        // Seven entries were cut off. Each distinct message logs once: the coroutine's own
        // error, and the host's notes when an argument or the scheduler could not even start.
        assert!(
            (1..=3).contains(&cut_off),
            "cut-off entries log once each kind: {cut_off}"
        );

        host.begin_frame();
        let before = host
            .object(REFERENCE)
            .unwrap()
            .get::<i64>("runs")
            .unwrap_or(0);
        host.activate(REFERENCE, PLAYER_REFERENCE);
        let after = host
            .object(REFERENCE)
            .unwrap()
            .get::<i64>("runs")
            .unwrap_or(0);
        assert_eq!(
            after,
            before + 1,
            "a new frame makes the budget whole again"
        );
    }

    #[test]
    fn a_fault_repeated_by_an_update_logs_once_and_is_counted() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("nagger.luau"),
            class(
                "Nagger",
                "ObjectReference",
                "",
                "Script[\"OnInit\"] = function(self) self.__runtime:call_method(self, \"RegisterForUpdate\", {0.1}) end\n\
                 Script[\"OnUpdate\"] = function(self) self.__runtime:call_method(nil, \"GetStage\", {}) end\n",
            ),
        )
        .unwrap();
        let mut forms = base_forms();
        attach(&mut forms, REFERENCE, "nagger", vec![]);
        let mut host = host_with(dir.path(), forms, false);
        host.attach_reference(REFERENCE);
        for _ in 0..30 {
            host.advance(0.1);
        }
        let lines = errors(&host);
        assert_eq!(lines.len(), 1, "one line, not one per tick: {lines:?}");
        let counts: Table = host.runtime().get("fault_counts").unwrap();
        assert!(counts.get::<i64>(lines[0].clone()).unwrap() >= 25);
    }

    #[test]
    fn a_legacy_reader_database_is_refused_and_an_in_house_one_opens() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("skyrim_world.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scripts(form_id INTEGER NOT NULL, script_name TEXT NOT NULL, \
                 vmad BLOB NOT NULL, properties_json TEXT);",
            )
            .unwrap();
        drop(connection);
        let error = SqliteForms::open(&path).err().expect("refused");
        assert!(error.contains("legacy record reader"), "{error}");

        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE script_properties(form_id INTEGER); \
                 CREATE TABLE script_property_values(form_id INTEGER);",
            )
            .unwrap();
        drop(connection);
        assert!(SqliteForms::open(&path).is_ok());
    }

    #[test]
    fn a_quest_script_does_not_attach_to_a_reference() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "DBRecurringControlQuestScript",
            vec![],
        );
        let mut host = host_with(dir.path(), forms, true);
        let report = host.attach_reference(REFERENCE);
        assert!(report.attached.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(
            report.skipped[0]
                .1
                .contains("does not extend ObjectReference")
        );
        assert!(host.object(REFERENCE).is_none());
    }

    #[test]
    fn base_properties_apply_first_and_the_reference_overrides_them() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        forms.bases.insert(REFERENCE, 0x9000);
        attach(
            &mut forms,
            0x9000,
            "defaultUnlockOnLoad",
            vec![ScriptProperty {
                name: "unlockStage".into(),
                value: PropertyValue::Int(5),
            }],
        );
        attach(
            &mut forms,
            REFERENCE,
            "defaultUnlockOnLoad",
            vec![ScriptProperty {
                name: "unlockStage".into(),
                value: PropertyValue::Int(7),
            }],
        );
        let mut host = host_with(dir.path(), forms, true);
        let report = host.attach_reference(REFERENCE);
        assert_eq!(report.attached.len(), 1, "one script, merged: {report:?}");
        let object = host.object(REFERENCE).unwrap();
        assert_eq!(object.get::<i64>("::unlockStage_var").unwrap(), 7);
    }
}
