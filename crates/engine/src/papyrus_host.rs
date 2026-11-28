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
/// Type code of a property that holds no value (`None`). It has no value rows.
const VMAD_NONE: i64 = 0;
/// An object property whose alias id is this value is not bound to a quest alias. The
/// converter stores the signed 16-bit alias id `-1` as `0xFFFF`.
const VMAD_NO_ALIAS: u16 = u16::MAX;
/// The alias types the engine declares natively, as `__types` spells them in lowercase.
/// Any other type that derives from `Alias` is a script with its own property values.
const BASE_ALIAS_TYPES: [&str; 3] = ["alias", "referencealias", "locationalias"];

/// `VMAD.Property.Flags` as the converter writes them to `script_properties.status`
/// (xEdit 053da98 `wbDefinitionsTES5.pas`, the `Flags` enum of a VMAD property: `1 = Edited`,
/// `3 = Removed`; `0` and `2` are unnamed there). The value is stored as the file holds it.
const VMAD_STATUS_EDITED: i64 = 1;
const VMAD_STATUS_REMOVED: i64 = 3;

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

/// What a script property row says about itself, before any value is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyStatus {
    /// The record sets this property (`VMAD.Property.Flags` 1, Edited). Seed its value.
    Edited,
    /// The record removed its override (`VMAD.Property.Flags` 3, Removed). The row still
    /// carries a value. It must not be seeded: an earlier owner's value, or the script's
    /// own default, stays. The value rows are still read and checked: `fault` says what is
    /// damaged in them, so the host can log it.
    Removed { fault: Option<String> },
    /// The row cannot be read as retail data: a column that its property type needs is
    /// NULL, a count or range is wrong, or the status or type is not a defined value. The
    /// reason is logged and the property is not seeded. Nothing is invented in its place.
    Unreadable(String),
}

/// One script property as the world database stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptProperty {
    pub name: String,
    pub value: PropertyValue,
    pub status: PropertyStatus,
}

impl ScriptProperty {
    /// A property its record sets ([`PropertyStatus::Edited`]).
    pub fn edited(name: impl Into<String>, value: PropertyValue) -> Self {
        Self {
            name: name.into(),
            value,
            status: PropertyStatus::Edited,
        }
    }

    fn unreadable(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: PropertyValue::None,
            status: PropertyStatus::Unreadable(reason.into()),
        }
    }
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

impl PropertyValue {
    /// Whether this value, or an array element of it, is an object bound to a quest alias.
    fn has_alias_binding(&self) -> bool {
        match self {
            Self::Object {
                alias: Some(alias), ..
            } => *alias != VMAD_NO_ALIAS,
            Self::Array(items) => items.iter().any(Self::has_alias_binding),
            Self::None
            | Self::Object { alias: None, .. }
            | Self::Str(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::Bool(_) => false,
        }
    }
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
    /// Rows this source could not read since the last call. The host logs each one.
    fn take_faults(&self) -> Vec<String> {
        Vec::new()
    }
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
        for fault in self.forms.take_faults() {
            self.note("error", fault);
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
        for (position, property) in props.iter().enumerate() {
            match &property.status {
                // A later Edited row replaces this one (a reference over its base record).
                // The replaced row must not run: its alias binding would fix the class of
                // a shared alias object that no property holds.
                PropertyStatus::Edited
                    if props[position + 1..].iter().any(|later| {
                        later.status == PropertyStatus::Edited
                            && later.name.eq_ignore_ascii_case(&property.name)
                    }) =>
                {
                    continue;
                }
                PropertyStatus::Edited => {}
                // xEdit 053da98 `wbDefinitionsTES5.pas` names VMAD property flag 3
                // `Removed`. A removed row sets nothing, so the script's own default stays
                // (#1052). When the base record also sets the property, the base value
                // stays; that merge is unverified against retail.
                PropertyStatus::Removed { fault } => {
                    if let Some(fault) = fault {
                        self.note(
                            "error",
                            format!(
                                "script {script} on {reference:08X}: removed property {} has damaged value rows ({fault}); a removed property is not set",
                                property.name
                            ),
                        );
                    }
                    continue;
                }
                PropertyStatus::Unreadable(reason) => {
                    self.note(
                        "error",
                        format!(
                            "script {script} on {reference:08X}: property {} is unreadable ({reason}); it is not set",
                            property.name
                        ),
                    );
                    continue;
                }
            }
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
            let declared = if property.value.has_alias_binding() {
                self.declared_type(object, &property.name)
            } else {
                None
            };
            match self.property_value(&property.value, script, &property.name, declared.as_deref())
            {
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

    /// The Papyrus type a script declares for an auto property, lowercase and without the
    /// array suffix. Converted scripts record it as `__types["::<name>_var"]`, the backing
    /// variable of the property. The class chain is searched, because a derived script
    /// inherits properties its parent declares. `None` when no class in the chain names it.
    fn declared_type(&self, object: &Table, name: &str) -> Option<String> {
        let class_named: Function = self.runtime.get("class_named").ok()?; // silent-ok: a runtime without class lookup cannot name a type.
        let identifier_key: Function = self.runtime.get("identifier_key").ok()?; // silent-ok: a runtime without key lookup cannot name a type.
        let variable = format!("::{name}_var");
        let mut types: Option<Table> = object.get("__types").ok().flatten(); // silent-ok: a script without type metadata declares no types.
        let mut parent: Option<String> = object.get("__parent").ok().flatten(); // silent-ok: a script without a parent ends the search.
        for _ in 0..64 {
            if let Some(table) = &types
                && let Ok(Some(key)) = identifier_key.call::<Option<String>>((
                    self.runtime.clone(),
                    table.clone(),
                    variable.clone(),
                ))
                && let Ok(Some(declared)) = table.get::<Option<String>>(key)
            {
                let lowered = declared.to_lowercase();
                return Some(lowered.strip_suffix("[]").unwrap_or(&lowered).to_string());
            }
            let class: Option<Table> = parent
                .as_deref()
                .and_then(|name| class_named.call((self.runtime.clone(), name)).ok()); // silent-ok: an unknown parent ends the search.
            let class = class?;
            types = class.get("__types").ok().flatten(); // silent-ok: a class without type metadata declares no types.
            parent = class.get("__parent").ok().flatten(); // silent-ok: a class without a parent ends the search.
        }
        None
    }

    /// Whether the class is `ancestor` or derives from it, following `__parent`.
    fn inherits(&self, class_name: &str, ancestor: &str) -> mlua::Result<bool> {
        let inherits: Function = self.runtime.get("inherits")?;
        inherits.call((self.runtime.clone(), class_name, ancestor))
    }

    /// A new object of an alias class, which must set no instance fields of its own.
    /// [`Self::alias_object`] changes the class of a shared object by swapping only its
    /// metatable, from `Alias` to a subtype. That assumes `new` of every engine alias
    /// class sets nothing but the runtime's own two keys, so no field can be missing after
    /// the swap. A converted class that gains a field is refused with an error.
    fn new_alias_instance(&self, class: &Table, declared: &str) -> mlua::Result<Table> {
        let new: Function = class.get("new")?;
        let object: Table = new.call(self.runtime.clone())?;
        for pair in object.pairs::<String, Value>() {
            let (key, _) = pair?;
            if key != "__runtime" && key != "__state" {
                return Err(mlua::Error::runtime(format!(
                    "alias class {declared} sets the instance field {key}; a shared alias object cannot hold it"
                )));
            }
        }
        Ok(object)
    }

    /// The object for one quest alias. Properties with compatible declared types share it,
    /// so Papyrus `==` is true for the same alias. It stores the quest and alias ids for
    /// `ReferenceAlias.GetReference`, which reads the current fill in
    /// [`WorldState::aliases`]. One property may declare `Alias` and another a subtype of
    /// it; the object then takes the subtype's class. `None` when two declared types for one
    /// alias are unrelated (`ReferenceAlias` and `LocationAlias`).
    fn alias_object(&self, quest: u32, alias: u16, declared: &str) -> mlua::Result<Option<Table>> {
        let cache: Table = match self.runtime.get::<Option<Table>>("alias_objects")? {
            Some(table) => table,
            None => {
                let table = self.lua.create_table()?;
                self.runtime.set("alias_objects", table.clone())?;
                table
            }
        };
        let class_named: Function = self.runtime.get("class_named")?;
        let class: Table = class_named
            .call::<Option<Table>>((self.runtime.clone(), declared))?
            .ok_or_else(|| mlua::Error::runtime(format!("alias class {declared} not found")))?;
        // Also run for an upgrade, so a subtype that sets a field is refused there too.
        let fresh = self.new_alias_instance(&class, declared)?;
        let key = format!("{quest:08X}:{alias}");
        if let Some(object) = cache.get::<Option<Table>>(key.as_str())? {
            let held: String = object.get("__name")?;
            if self.inherits(&held, declared)? {
                return Ok(Some(object));
            }
            if self.inherits(declared, &held)? {
                // The metatable swap is safe by the check in `new_alias_instance`.
                object.set_metatable(Some(class));
                return Ok(Some(object));
            }
            return Ok(None);
        }
        let object = fresh;
        object.raw_set("__form_id", quest)?;
        object.raw_set("__alias_id", alias)?;
        cache.set(key.as_str(), object.clone())?;
        Ok(Some(object))
    }

    fn property_value(
        &self,
        value: &PropertyValue,
        script: &str,
        name: &str,
        declared: Option<&str>,
    ) -> mlua::Result<Value> {
        Ok(match value {
            PropertyValue::None => Value::Nil,
            PropertyValue::Int(value) => value.into_lua(&self.lua)?,
            PropertyValue::Float(value) => Value::Number(*value),
            PropertyValue::Bool(value) => Value::Boolean(*value),
            PropertyValue::Str(value) => self.lua.create_string(value)?.into_lua(&self.lua)?,
            PropertyValue::Object {
                form_id,
                alias: Some(alias),
            } if *alias != VMAD_NO_ALIAS => {
                // Retail: an object value with an alias id other than -1 is bound to that
                // alias of the quest the FormID names (xEdit 053da98 `wbDefinitionsTES5.pas`,
                // VMAD Object Union: FormID, Alias). The property holds the alias. A
                // `ReferenceAlias` extends `Alias` in the Papyrus class tree, and its
                // `GetReference` returns the current fill, or None for an empty alias
                // (Creation Kit Papyrus reference, GetReference - ReferenceAlias).
                // Only the engine's own alias types are built. A derived alias script needs
                // the property values from the quest's alias VMAD section, which the world
                // database does not hold. An object with empty variables would be a
                // stand-in, so such a property stays None and says so.
                let quest = *form_id;
                let unset = |reason: String| {
                    self.note(
                        "warn",
                        format!(
                            "script {script} property {name}: alias {alias} of form {quest:08X} {reason}; the property stays None"
                        ),
                    );
                    Ok::<Value, mlua::Error>(Value::Nil)
                };
                let Some(declared) = declared else {
                    return unset("is bound, but the script declares no type for it".into());
                };
                if !BASE_ALIAS_TYPES.contains(&declared) {
                    return unset(if self.inherits(declared, "Alias")? {
                        format!(
                            "needs the alias script {declared}, and quest alias scripts are not attached yet"
                        )
                    } else {
                        format!("is bound to type {declared}, which is not an alias type")
                    });
                }
                if self.forms.record_type(quest).as_deref() != Some("QUST") {
                    return unset("names a form that is not a quest in the world database".into());
                }
                match self.alias_object(quest, *alias, declared)? {
                    Some(object) => Value::Table(object),
                    None => {
                        return unset(format!(
                            "is declared {declared} here and an unrelated alias type elsewhere"
                        ));
                    }
                }
            }
            PropertyValue::Object { form_id, .. } => {
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
                    table.set(
                        index + 1,
                        self.property_value(item, script, name, declared)?,
                    )?;
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

// ---- Property row decoding -----------------------------------------------------------

/// One `script_property_values` row exactly as stored. A NULL column stays `None`: no
/// column is ever replaced by `0`, an empty string or `false`.
#[derive(Debug, Clone, Default, PartialEq)]
struct ValueRow {
    /// `value_index`: the element's position in the property.
    index: i64,
    object: Option<i64>,
    alias: Option<i64>,
    string: Option<String>,
    integer: Option<i64>,
    float_bits: Option<i64>,
    boolean: Option<i64>,
}

/// Decode one value row of a scalar property type. The column that matches the type must
/// hold a value. A NULL or out-of-range column is corrupt data and is reported, never
/// turned into a default (#1095). The converter writes every column it needs for a type
/// (`crates/converter/src/esm/inhouse_script_properties.rs`).
fn decode_scalar(kind: i64, row: &ValueRow) -> Result<PropertyValue, String> {
    match kind {
        VMAD_OBJECT => {
            let object = row
                .object
                .ok_or_else(|| "object_form_id is NULL".to_string())?;
            let form_id = u32::try_from(object)
                .map_err(|_| format!("object_form_id {object} is not a 32-bit form id"))?;
            let alias = row.alias.ok_or_else(|| "alias_id is NULL".to_string())?;
            let alias = u16::try_from(alias)
                .map_err(|_| format!("alias_id {alias} is not a 16-bit alias id"))?;
            Ok(PropertyValue::Object {
                form_id,
                alias: Some(alias),
            })
        }
        VMAD_STRING => row
            .string
            .clone()
            .map(PropertyValue::Str)
            .ok_or_else(|| "string_value is NULL".to_string()),
        VMAD_INT => {
            let integer = row
                .integer
                .ok_or_else(|| "integer_value is NULL".to_string())?;
            i32::try_from(integer)
                .map(|value| PropertyValue::Int(i64::from(value)))
                .map_err(|_| format!("integer_value {integer} is outside a 32-bit Int"))
        }
        VMAD_FLOAT => {
            let bits = row
                .float_bits
                .ok_or_else(|| "float_bits is NULL".to_string())?;
            u32::try_from(bits)
                .map(|bits| PropertyValue::Float(f64::from(f32::from_bits(bits))))
                .map_err(|_| format!("float_bits {bits} is not 32 bits"))
        }
        VMAD_BOOL => match row.boolean {
            Some(0) => Ok(PropertyValue::Bool(false)),
            Some(1) => Ok(PropertyValue::Bool(true)),
            Some(other) => Err(format!("boolean_value {other} is neither 0 nor 1")),
            None => Err("boolean_value is NULL".to_string()),
        },
        other => Err(format!("property type {other} has no value decoder")),
    }
}

/// Turn one `script_properties` row and its value rows into a [`ScriptProperty`].
/// `status` is checked against the defined VMAD values and an undefined one is rejected.
/// A Removed property keeps no value; damage in its rows is reported with it.
fn decode_property(
    name: String,
    kind: i64,
    status: i64,
    count: i64,
    rows: Vec<Result<ValueRow, String>>,
) -> ScriptProperty {
    match status {
        VMAD_STATUS_EDITED => match decode_value(kind, count, rows) {
            Ok(value) => ScriptProperty::edited(name, value),
            Err(reason) => ScriptProperty::unreadable(name, reason),
        },
        VMAD_STATUS_REMOVED => ScriptProperty {
            name,
            value: PropertyValue::None,
            status: PropertyStatus::Removed {
                fault: decode_value(kind, count, rows).err(),
            },
        },
        other => {
            let reason = format!("status {other} is not a defined VMAD property status");
            ScriptProperty::unreadable(name, reason)
        }
    }
}

/// The value of a property of type `kind`. An undefined type is rejected. The value count
/// the header records must match the rows found, and their indexes must run from 0 with no
/// gap or repeat, so a missing element is reported and the others never move into its place.
fn decode_value(
    kind: i64,
    count: i64,
    rows: Vec<Result<ValueRow, String>>,
) -> Result<PropertyValue, String> {
    let is_array = (VMAD_ARRAY_FIRST..=VMAD_ARRAY_LAST).contains(&kind);
    let scalar_kind = if is_array {
        kind - VMAD_ARRAY_OFFSET
    } else {
        kind
    };
    if kind != VMAD_NONE && !is_array && !(VMAD_OBJECT..=VMAD_BOOL).contains(&kind) {
        return Err(format!(
            "property type {kind} is not a defined VMAD property type"
        ));
    }
    let rows: Vec<ValueRow> = rows
        .into_iter()
        .collect::<Result<_, _>>()
        .map_err(|error| format!("a value row cannot be read: {error}"))?;
    if i64::try_from(rows.len()) != Ok(count) {
        return Err(format!(
            "value_count is {count} but {} value rows exist",
            rows.len()
        ));
    }
    if kind == VMAD_NONE {
        return if rows.is_empty() {
            Ok(PropertyValue::None)
        } else {
            Err("a None property has value rows".to_string())
        };
    }
    if !is_array && rows.len() != 1 {
        return Err(format!(
            "a scalar property has {} value rows, not 1",
            rows.len()
        ));
    }
    let mut elements = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        if i64::try_from(position) != Ok(row.index) {
            return Err(format!(
                "value_index {} is at position {position}; indexes must run from 0 with no gap or repeat",
                row.index
            ));
        }
        let element = decode_scalar(scalar_kind, row)
            .map_err(|error| format!("value row {position}: {error}"))?;
        elements.push(element);
    }
    Ok(if is_array {
        PropertyValue::Array(elements)
    } else {
        // The row count check above leaves exactly one element for a scalar.
        elements.swap_remove(0)
    })
}

// ---- Sqlite form source --------------------------------------------------------------

/// A [`FormSource`] over the converted pack's `skyrim_world.db`, opened read-only.
pub struct SqliteForms {
    connection: rusqlite::Connection,
    scripted: HashSet<u32>,
    /// Rows that could not be read, kept until the host takes them to log.
    faults: std::cell::RefCell<Vec<String>>,
}

impl SqliteForms {
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let mut faults = Vec::new();
        let scripted = {
            let mut statement = connection
                .prepare("SELECT DISTINCT form_id FROM scripts")
                .map_err(|error| format!("{} has no scripts table: {error}", path.display()))?;
            let rows = statement
                .query_map([], |row| row.get::<_, u32>(0))
                .map_err(|error| error.to_string())?;
            let mut scripted = HashSet::new();
            for row in rows {
                match row {
                    Ok(form_id) => {
                        scripted.insert(form_id);
                    }
                    Err(error) => faults.push(format!(
                        "a scripts row has an unreadable form_id; its scripts do not attach: {error}"
                    )),
                }
            }
            scripted
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
            faults: std::cell::RefCell::new(faults),
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
        let fault = |error: rusqlite::Error| {
            self.faults.borrow_mut().push(format!(
                "a script name of {form_id:08X} cannot be read; that script does not attach: {error}"
            ));
        };
        let mut statement = match self.connection.prepare_cached(
            "SELECT script_name FROM scripts WHERE form_id = ?1 ORDER BY script_name",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                fault(error);
                return Vec::new();
            }
        };
        let rows = match statement.query_map([form_id], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(error) => {
                fault(error);
                return Vec::new();
            }
        };
        let mut names = Vec::new();
        for row in rows {
            match row {
                Ok(name) => names.push(name),
                Err(error) => fault(error),
            }
        }
        names
    }

    fn script_properties(&self, form_id: u32, script_name: &str) -> Vec<ScriptProperty> {
        // A statement or row that cannot be read becomes an unreadable property, which the
        // host logs. It is never dropped, and its columns are never replaced by zero values.
        let mut properties = match self.connection.prepare_cached(
            "SELECT property_index, name, property_type, status, value_count \
             FROM script_properties \
             WHERE form_id = ?1 AND script_name = ?2 ORDER BY property_index",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                return vec![ScriptProperty::unreadable(
                    "<script_properties>",
                    format!("cannot read script_properties: {error}"),
                )];
            }
        };
        let mut values = match self.connection.prepare_cached(
            "SELECT object_form_id, alias_id, string_value, integer_value, float_bits, boolean_value, \
             value_index FROM script_property_values \
             WHERE form_id = ?1 AND script_name = ?2 AND property_index = ?3 ORDER BY value_index",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                return vec![ScriptProperty::unreadable(
                    "<script_property_values>",
                    format!("cannot read script_property_values: {error}"),
                )];
            }
        };
        let headers = match properties.query_map(rusqlite::params![form_id, script_name], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        }) {
            Ok(headers) => headers,
            Err(error) => {
                return vec![ScriptProperty::unreadable(
                    "<script_properties>",
                    error.to_string(),
                )];
            }
        };
        headers
            .map(|header| {
                let (index, name, kind, status, count) = match header {
                    Ok(header) => header,
                    Err(error) => {
                        return ScriptProperty::unreadable(
                            "<unreadable property row>",
                            error.to_string(),
                        );
                    }
                };
                // A Removed property's rows are read too, so damage in them is reported.
                let rows: Vec<Result<ValueRow, String>> =
                    match values.query_map(rusqlite::params![form_id, script_name, index], |row| {
                        Ok(ValueRow {
                            object: row.get(0)?,
                            alias: row.get(1)?,
                            string: row.get(2)?,
                            integer: row.get(3)?,
                            float_bits: row.get(4)?,
                            boolean: row.get(5)?,
                            index: row.get(6)?,
                        })
                    }) {
                        Ok(rows) => rows
                            .map(|row| row.map_err(|error| error.to_string()))
                            .collect(),
                        Err(error) => vec![Err(error.to_string())],
                    };
                decode_property(name, kind, status, count, rows)
            })
            .collect()
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

    fn take_faults(&self) -> Vec<String> {
        self.faults.take()
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
        // Alias classes as the converter emits them: `Alias` has no parent, and
        // `ReferenceAlias` and `LocationAlias` extend it. `heroAlias` is an alias script.
        write("alias", natives("Alias", "", &[]));
        write(
            "referencealias",
            natives("ReferenceAlias", "Alias", &[("GetReference", 0)]),
        );
        write("locationalias", natives("LocationAlias", "Alias", &[]));
        write("heroalias", natives("heroAlias", "ReferenceAlias", &[]));
        // A reference script with alias-typed, array-of-alias and plain object properties.
        write(
            "aliasuser",
            class(
                "aliasUser",
                "ObjectReference",
                "",
                &("Script.__types[\"::Hero_var\"] = \"referencealias\"\n\
                   Script.__types[\"::Boss_var\"] = \"referencealias\"\n\
                   Script.__types[\"::Guards_var\"] = \"referencealias[]\"\n\
                   Script.__types[\"::Plain_var\"] = \"objectreference\"\n\
                   Script.__types[\"::Anyone_var\"] = \"alias\"\n\
                   Script.__types[\"::Place_var\"] = \"locationalias\"\n\
                   Script.__types[\"::Scripted_var\"] = \"heroalias\"\n"
                    .to_string()
                    + &property("Hero")
                    + &property("Boss")
                    + &property("Guards")
                    + &property("Plain")
                    + &property("Untyped")
                    + &property("Anyone")
                    + &property("Place")
                    + &property("Scripted")),
            ),
        );
        // A script whose properties have defaults, to show what a removed override leaves.
        write(
            "defaultsdemo",
            class(
                "defaultsDemo",
                "ObjectReference",
                "",
                &("function Script.new(runtime) \
                   local self = setmetatable({ __runtime = runtime }, Script) \
                   self[\"::level_var\"] = 42 self[\"::flag_var\"] = true return self end\n"
                    .to_string()
                    + &property("level")
                    + &property("flag")),
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

    fn host_with_source(dir: &Path, forms: Rc<dyn FormSource>) -> PapyrusHost {
        PapyrusHost::new(HostConfig::new(dir), forms).unwrap()
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
        ScriptProperty::edited(
            name,
            PropertyValue::Object {
                form_id,
                alias: Some(u16::MAX),
            },
        )
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
                ScriptProperty::edited("unlockStage", PropertyValue::Int(10)),
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
            vec![ScriptProperty::edited(
                "startOpen",
                PropertyValue::Bool(false),
            )],
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

    /// The VMAD property rules on a real converted pack, which the test only reads. Set
    /// `MUDCRAB_PAPYRUS_PACK` to a pack folder. The form ids come from #1051, #1052 and
    /// that pack's database.
    #[test]
    fn real_pack_vmad_properties() {
        let Some(pack) = std::env::var_os("MUDCRAB_PAPYRUS_PACK") else {
            return;
        };
        let pack = Path::new(&pack);
        let forms = SqliteForms::open(&pack.join("skyrim_world.db")).unwrap();

        // #1095: no retail row is refused by the stricter reader.
        let (mut read, mut removed, mut aliased) = (0, 0, 0);
        let mut unreadable = Vec::new();
        for &form in &forms.scripted {
            for script in forms.script_names(form) {
                for property in forms.script_properties(form, &script) {
                    read += 1;
                    aliased += usize::from(property.value.has_alias_binding());
                    match property.status {
                        PropertyStatus::Edited => {}
                        PropertyStatus::Removed { fault } => {
                            removed += 1;
                            unreadable.extend(
                                fault.map(|reason| format!("{form:08X} {script} {reason}")),
                            );
                        }
                        PropertyStatus::Unreadable(reason) => {
                            unreadable.push(format!("{form:08X} {script} {reason}"));
                        }
                    }
                }
            }
        }
        println!("properties {read}, removed {removed}, with an alias binding {aliased}");
        assert!(read > 0);
        assert_eq!(unreadable, Vec::<String>::new());
        assert_eq!(forms.take_faults(), Vec::<String>::new());

        let mut host =
            PapyrusHost::new(HostConfig::new(pack.join("scripts")), Rc::new(forms)).unwrap();
        // The retail alias classes set no instance fields, which `alias_object` relies on.
        // This fails if `Alias` or a subtype gains one.
        let class_named: Function = host.runtime().get("class_named").unwrap();
        for name in BASE_ALIAS_TYPES {
            let class: Table = class_named.call((host.runtime().clone(), name)).unwrap();
            host.new_alias_instance(&class, name).unwrap();
        }

        let script_object = |host: &PapyrusHost, form: u32, script: &str| -> Table {
            let objects: Table = host.runtime().get("objects").unwrap();
            let list: Table = objects.get(form).unwrap();
            list.sequence_values::<Table>()
                .map(Result::unwrap)
                .find(|o| {
                    o.get::<String>("__name")
                        .unwrap()
                        .eq_ignore_ascii_case(script)
                })
                .unwrap_or_else(|| panic!("{script} is not attached to {form:08X}"))
        };

        // #1051: a scalar reference alias reads the fill, through the converted ReferenceAlias.
        host.attach_reference(0x0001_5514);
        let wizard = script_object(&host, 0x0001_5514, "dunMarkarthWizard_UpdateFactions");
        let actor: Table = wizard.get("::Actor1_var").unwrap();
        assert_eq!(actor.get::<String>("__name").unwrap(), "ReferenceAlias");
        assert_eq!(actor.get::<u32>("__form_id").unwrap(), 0x0008_7105);
        assert_eq!(actor.get::<u16>("__alias_id").unwrap(), 27);
        assert!(matches!(get_reference(&host, &actor), Value::Nil));
        host.world().aliases.insert((0x0008_7105, 27), 0x0001_5514);
        assert!(matches!(get_reference(&host, &actor), Value::Table(_)));

        // #1051: a quest script's alias properties, reference and location aliases.
        host.attach_quest(0x0001_5D24);
        let quest = script_object(&host, 0x0001_5D24, "QF_TGRDU_00015D24");
        let vex: Table = quest.get("::Alias_VexAlias_var").unwrap();
        assert_eq!(vex.get::<u16>("__alias_id").unwrap(), 6);
        let city: Table = quest.get("::Alias_City_var").unwrap();
        assert_eq!(city.get::<String>("__name").unwrap(), "LocationAlias");

        // #1052: the reference removed `myHMface05`. Its row still names 0403913F, which is
        // not seeded, so the base record's 04037AE2 stays.
        host.attach_reference(0x0403_9140);
        let face = script_object(&host, 0x0403_9140, "DLC2HermaeusMoraFaceFXSCRIPT");
        let target: Table = face.get("::myHMface05_var").unwrap();
        assert_eq!(target.get::<u32>("__form_id").unwrap(), 0x0403_7AE2);

        let unreadable_lines: Vec<String> = errors(&host)
            .into_iter()
            .filter(|line| {
                ["is unreadable", "damaged value rows", "cannot be read"]
                    .iter()
                    .any(|mark| line.contains(mark))
            })
            .collect();
        assert_eq!(unreadable_lines, Vec::<String>::new());
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
        let mut host = PapyrusHost::new(config, Rc::new(forms)).unwrap();
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
            vec![ScriptProperty::edited("unlockStage", PropertyValue::Int(5))],
        );
        attach(
            &mut forms,
            REFERENCE,
            "defaultUnlockOnLoad",
            vec![ScriptProperty::edited("unlockStage", PropertyValue::Int(7))],
        );
        let mut host = host_with(dir.path(), forms, true);
        let report = host.attach_reference(REFERENCE);
        assert_eq!(report.attached.len(), 1, "one script, merged: {report:?}");
        let object = host.object(REFERENCE).unwrap();
        assert_eq!(object.get::<i64>("::unlockStage_var").unwrap(), 7);
    }

    // ---- #1052 removed properties, #1095 NULL columns, #1051 alias-bound objects ----------

    /// A value row with only the column that `kind` needs filled.
    fn filled_row(kind: i64) -> ValueRow {
        let mut row = ValueRow::default();
        match kind {
            VMAD_OBJECT => {
                row.object = Some(0x1234);
                row.alias = Some(i64::from(VMAD_NO_ALIAS));
            }
            VMAD_STRING => row.string = Some("text".into()),
            VMAD_INT => row.integer = Some(0),
            VMAD_FLOAT => row.float_bits = Some(i64::from(1.5_f32.to_bits())),
            VMAD_BOOL => row.boolean = Some(0),
            other => panic!("no fixture for type {other}"),
        }
        row
    }

    /// Number rows by position, as the converter writes `value_index`.
    fn indexed(rows: Vec<ValueRow>) -> Vec<ValueRow> {
        (0..)
            .zip(rows)
            .map(|(index, row)| ValueRow { index, ..row })
            .collect()
    }

    fn decoded(kind: i64, status: i64, count: i64, rows: Vec<ValueRow>) -> ScriptProperty {
        decode_property(
            "p".into(),
            kind,
            status,
            count,
            indexed(rows).into_iter().map(Ok).collect(),
        )
    }

    const REMOVED: PropertyStatus = PropertyStatus::Removed { fault: None };

    #[test]
    fn a_stored_zero_empty_string_or_false_is_data_and_stays() {
        // 0, "" and false are legitimate when the column holds them. Only NULL is absent.
        let mut row = filled_row(VMAD_STRING);
        row.string = Some(String::new());
        assert_eq!(
            decoded(VMAD_STRING, 1, 1, vec![row]).value,
            PropertyValue::Str(String::new())
        );
        assert_eq!(
            decoded(VMAD_INT, 1, 1, vec![filled_row(VMAD_INT)]).value,
            PropertyValue::Int(0)
        );
        assert_eq!(
            decoded(VMAD_BOOL, 1, 1, vec![filled_row(VMAD_BOOL)]).value,
            PropertyValue::Bool(false)
        );
        let mut row = filled_row(VMAD_FLOAT);
        row.float_bits = Some(0);
        assert_eq!(
            decoded(VMAD_FLOAT, 1, 1, vec![row]).value,
            PropertyValue::Float(0.0)
        );
    }

    #[test]
    fn a_null_value_column_is_unreadable_never_a_default_value() {
        for kind in [VMAD_OBJECT, VMAD_STRING, VMAD_INT, VMAD_FLOAT, VMAD_BOOL] {
            let mut row = filled_row(kind);
            match kind {
                VMAD_OBJECT => row.object = None,
                VMAD_STRING => row.string = None,
                VMAD_INT => row.integer = None,
                VMAD_FLOAT => row.float_bits = None,
                _ => row.boolean = None,
            }
            let property = decoded(kind, 1, 1, vec![row]);
            assert!(
                matches!(&property.status, PropertyStatus::Unreadable(reason) if reason.contains("NULL")),
                "type {kind}: {property:?}"
            );
            assert_eq!(property.value, PropertyValue::None, "type {kind}");
        }
        // The alias column of an object row is NULL-checked too.
        let mut row = filled_row(VMAD_OBJECT);
        row.alias = None;
        assert!(matches!(
            decoded(VMAD_OBJECT, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
    }

    #[test]
    fn one_null_array_element_makes_the_whole_array_unreadable() {
        let mut hole = filled_row(VMAD_INT);
        hole.integer = None;
        let property = decoded(
            VMAD_INT + VMAD_ARRAY_OFFSET,
            1,
            3,
            vec![filled_row(VMAD_INT), hole, filled_row(VMAD_INT)],
        );
        assert!(
            matches!(&property.status, PropertyStatus::Unreadable(reason) if reason.contains("value row 1")),
            "{property:?}"
        );
        // An array of three fully filled rows reads as three elements.
        let property = decoded(
            VMAD_INT + VMAD_ARRAY_OFFSET,
            1,
            3,
            vec![
                filled_row(VMAD_INT),
                filled_row(VMAD_INT),
                filled_row(VMAD_INT),
            ],
        );
        assert_eq!(property.status, PropertyStatus::Edited);
        assert_eq!(
            property.value,
            PropertyValue::Array(vec![PropertyValue::Int(0); 3])
        );
        // An empty array is data, not corruption.
        assert_eq!(
            decoded(VMAD_INT + VMAD_ARRAY_OFFSET, 1, 0, vec![]).value,
            PropertyValue::Array(vec![])
        );
    }

    #[test]
    fn out_of_range_columns_and_wrong_counts_are_unreadable() {
        let mut row = filled_row(VMAD_OBJECT);
        row.object = Some(-1);
        assert!(matches!(
            decoded(VMAD_OBJECT, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
        let mut row = filled_row(VMAD_OBJECT);
        row.alias = Some(0x1_0000);
        assert!(matches!(
            decoded(VMAD_OBJECT, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
        let mut row = filled_row(VMAD_INT);
        row.integer = Some(i64::from(i32::MAX) + 1);
        assert!(matches!(
            decoded(VMAD_INT, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
        let mut row = filled_row(VMAD_FLOAT);
        row.float_bits = Some(-1);
        assert!(matches!(
            decoded(VMAD_FLOAT, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
        let mut row = filled_row(VMAD_BOOL);
        row.boolean = Some(2);
        assert!(matches!(
            decoded(VMAD_BOOL, 1, 1, vec![row]).status,
            PropertyStatus::Unreadable(_)
        ));
        // A scalar with no value row, and a header count that disagrees with the rows.
        assert!(matches!(
            decoded(VMAD_INT, 1, 1, vec![]).status,
            PropertyStatus::Unreadable(_)
        ));
        assert!(matches!(
            decoded(
                VMAD_INT + VMAD_ARRAY_OFFSET,
                1,
                2,
                vec![filled_row(VMAD_INT)]
            )
            .status,
            PropertyStatus::Unreadable(_)
        ));
        // A None property carries no value rows.
        assert_eq!(
            decoded(VMAD_NONE, 1, 0, vec![]).status,
            PropertyStatus::Edited
        );
        assert!(matches!(
            decoded(VMAD_NONE, 1, 1, vec![filled_row(VMAD_INT)]).status,
            PropertyStatus::Unreadable(_)
        ));
    }

    #[test]
    fn an_undefined_status_or_type_is_rejected_not_guessed() {
        for status in [0, 2, 4, -1, 255] {
            let property = decoded(VMAD_INT, status, 1, vec![filled_row(VMAD_INT)]);
            assert!(
                matches!(&property.status, PropertyStatus::Unreadable(reason) if reason.contains("status")),
                "status {status}: {property:?}"
            );
        }
        for kind in [6, 7, 9, 10, 16, 17, -1, 100] {
            let property = decoded(kind, 1, 1, vec![filled_row(VMAD_INT)]);
            assert!(
                matches!(&property.status, PropertyStatus::Unreadable(reason) if reason.contains("type")),
                "type {kind}: {property:?}"
            );
        }
    }

    #[test]
    fn a_removed_status_wins_over_whatever_the_row_holds() {
        // A removed property still carries a value (a bool false, an object id). It never
        // reaches the script.
        let property = decoded(
            VMAD_BOOL,
            VMAD_STATUS_REMOVED,
            1,
            vec![filled_row(VMAD_BOOL)],
        );
        assert_eq!(property.status, REMOVED);
        assert_eq!(property.value, PropertyValue::None);
        // Damage under a Removed property is reported, and the property stays Removed.
        let property = decode_property(
            "p".into(),
            VMAD_INT,
            VMAD_STATUS_REMOVED,
            1,
            vec![Err("not even readable".into())],
        );
        assert!(
            matches!(&property.status, PropertyStatus::Removed { fault: Some(fault) } if fault.contains("not even readable")),
            "{property:?}"
        );
        for (count, rows) in [
            (1, vec![ValueRow::default()]),
            (2, vec![filled_row(VMAD_INT)]),
        ] {
            let property = decoded(VMAD_INT, VMAD_STATUS_REMOVED, count, rows);
            assert!(
                matches!(&property.status, PropertyStatus::Removed { fault: Some(_) }),
                "{property:?}"
            );
        }
    }

    #[test]
    fn a_gap_or_repeat_in_value_indexes_is_unreadable() {
        for indexes in [[0, 2, 3], [1, 2, 3], [0, 0, 1], [0, 1, 1], [-1, 0, 1]] {
            let rows = indexes
                .into_iter()
                .map(|index| {
                    Ok(ValueRow {
                        index,
                        ..filled_row(VMAD_INT)
                    })
                })
                .collect();
            let property = decode_property(
                "p".into(),
                VMAD_INT + VMAD_ARRAY_OFFSET,
                VMAD_STATUS_EDITED,
                3,
                rows,
            );
            assert!(
                matches!(&property.status, PropertyStatus::Unreadable(reason) if reason.contains("value_index")),
                "{indexes:?}: {property:?}"
            );
        }
        // A scalar's single row must sit at index 0 too.
        let row = ValueRow {
            index: 1,
            ..filled_row(VMAD_INT)
        };
        assert!(matches!(
            decode_property("p".into(), VMAD_INT, VMAD_STATUS_EDITED, 1, vec![Ok(row)]).status,
            PropertyStatus::Unreadable(_)
        ));
    }

    /// A tiny deterministic generator, so a fuzz failure reproduces from its seed.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        /// A column value: NULL a third of the time, else a value that is sometimes out of range.
        fn column(&mut self) -> Option<i64> {
            match self.below(6) {
                0 | 1 => None,
                2 => Some(0),
                3 => Some(1),
                4 => Some(i64::try_from(self.below(70_000)).unwrap()),
                _ => Some(i64::try_from(self.next()).unwrap() - (1 << 30)),
            }
        }
    }

    /// The oracle: a row decodes only when the column its type needs is present and in range.
    fn oracle(kind: i64, row: &ValueRow) -> bool {
        match kind {
            VMAD_OBJECT => {
                row.object.is_some_and(|v| u32::try_from(v).is_ok())
                    && row.alias.is_some_and(|v| u16::try_from(v).is_ok())
            }
            VMAD_STRING => row.string.is_some(),
            VMAD_INT => row.integer.is_some_and(|v| i32::try_from(v).is_ok()),
            VMAD_FLOAT => row.float_bits.is_some_and(|v| u32::try_from(v).is_ok()),
            VMAD_BOOL => matches!(row.boolean, Some(0 | 1)),
            _ => false,
        }
    }

    fn random_row(rng: &mut Lcg) -> ValueRow {
        ValueRow {
            index: 0,
            object: rng.column(),
            alias: rng.column(),
            string: (rng.below(3) != 0).then(|| "s".to_string()),
            integer: rng.column(),
            float_bits: rng.column(),
            boolean: rng.column(),
        }
    }

    #[test]
    fn fuzz_decoding_never_turns_a_null_or_bad_column_into_data() {
        let mut rng = Lcg(0x1095);
        for case in 0..20_000 {
            let scalar = 1 + i64::try_from(rng.below(5)).unwrap();
            let array = rng.below(2) == 0;
            let kind = if array {
                scalar + VMAD_ARRAY_OFFSET
            } else {
                scalar
            };
            let rows: Vec<ValueRow> = (0..if array { rng.below(4) } else { 1 })
                .map(|_| random_row(&mut rng))
                .collect();
            let all_good = rows.iter().all(|row| oracle(scalar, row));
            let property = decoded(
                kind,
                VMAD_STATUS_EDITED,
                i64::try_from(rows.len()).unwrap(),
                rows.clone(),
            );
            match property.status {
                PropertyStatus::Edited => {
                    assert!(
                        all_good,
                        "case {case}: type {kind} accepted bad rows {rows:?}"
                    );
                    let expected = if array { rows.len() } else { 1 };
                    match (&property.value, array) {
                        (PropertyValue::Array(items), true) => assert_eq!(items.len(), expected),
                        (_, false) => {}
                        (other, true) => panic!("case {case}: array decoded as {other:?}"),
                    }
                }
                PropertyStatus::Unreadable(_) => {
                    assert!(
                        !all_good,
                        "case {case}: type {kind} refused good rows {rows:?}"
                    );
                    assert_eq!(property.value, PropertyValue::None);
                }
                PropertyStatus::Removed { .. } => {
                    panic!("case {case}: an Edited row came back Removed")
                }
            }
            // A header that disagrees with the rows is always refused.
            let wrong = decoded(
                kind,
                VMAD_STATUS_EDITED,
                i64::try_from(rows.len()).unwrap() + 1,
                rows,
            );
            assert!(
                matches!(wrong.status, PropertyStatus::Unreadable(_)),
                "case {case}"
            );
        }
    }

    fn property_database(dir: &Path) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open(dir.join("skyrim_world.db")).unwrap();
        // The tables as `inhouse_script_properties.rs` creates them.
        connection
            .execute_batch(
                "CREATE TABLE scripts(form_id INTEGER NOT NULL, script_name TEXT NOT NULL, \
                 vmad BLOB NOT NULL, PRIMARY KEY(form_id, script_name)); \
                 CREATE TABLE script_properties(form_id INTEGER NOT NULL, script_name TEXT NOT NULL, \
                 property_index INTEGER NOT NULL, name TEXT NOT NULL, property_type INTEGER NOT NULL, \
                 status INTEGER NOT NULL, value_count INTEGER NOT NULL, \
                 PRIMARY KEY(form_id, script_name, property_index)) WITHOUT ROWID; \
                 CREATE TABLE script_property_values(form_id INTEGER NOT NULL, \
                 script_name TEXT NOT NULL, property_index INTEGER NOT NULL, \
                 value_index INTEGER NOT NULL, object_form_id INTEGER, alias_id INTEGER, \
                 string_value TEXT, integer_value INTEGER, float_bits INTEGER, boolean_value INTEGER, \
                 PRIMARY KEY(form_id, script_name, property_index, value_index)) WITHOUT ROWID;",
            )
            .unwrap();
        connection
            .execute("INSERT INTO scripts VALUES (?1, 'S', x'00')", [REFERENCE])
            .unwrap();
        connection
    }

    fn add_property(
        connection: &rusqlite::Connection,
        index: i64,
        name: &str,
        kind: i64,
        status: i64,
        rows: &[ValueRow],
    ) {
        connection
            .execute(
                "INSERT INTO script_properties VALUES (?1, 'S', ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    REFERENCE,
                    index,
                    name,
                    kind,
                    status,
                    i64::try_from(rows.len()).unwrap()
                ],
            )
            .unwrap();
        for (value_index, row) in rows.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO script_property_values VALUES (?1, 'S', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        REFERENCE,
                        index,
                        i64::try_from(value_index).unwrap(),
                        row.object,
                        row.alias,
                        row.string,
                        row.integer,
                        row.float_bits,
                        row.boolean
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn the_sqlite_reader_keeps_null_absent_and_reports_the_row() {
        let dir = TempDir::new().unwrap();
        let connection = property_database(dir.path());
        let null = ValueRow::default();
        let empty = ValueRow {
            string: Some(String::new()),
            ..ValueRow::default()
        };
        let zero = filled_row(VMAD_INT);
        let false_value = filled_row(VMAD_BOOL);
        let null_alias = ValueRow {
            object: Some(0x2000),
            ..ValueRow::default()
        };
        let object = ValueRow {
            alias: Some(i64::from(VMAD_NO_ALIAS)),
            ..null_alias.clone()
        };
        let hole = [zero.clone(), null.clone(), zero.clone()];
        let cases: [(&str, i64, i64, &[ValueRow]); 14] = [
            (
                "nullInt",
                VMAD_INT,
                VMAD_STATUS_EDITED,
                std::slice::from_ref(&null),
            ),
            (
                "nullString",
                VMAD_STRING,
                VMAD_STATUS_EDITED,
                std::slice::from_ref(&null),
            ),
            ("emptyString", VMAD_STRING, VMAD_STATUS_EDITED, &[empty]),
            (
                "zeroInt",
                VMAD_INT,
                VMAD_STATUS_EDITED,
                std::slice::from_ref(&zero),
            ),
            (
                "falseBool",
                VMAD_BOOL,
                VMAD_STATUS_EDITED,
                std::slice::from_ref(&false_value),
            ),
            (
                "nullObject",
                VMAD_OBJECT,
                VMAD_STATUS_EDITED,
                std::slice::from_ref(&null),
            ),
            ("nullAlias", VMAD_OBJECT, VMAD_STATUS_EDITED, &[null_alias]),
            ("object", VMAD_OBJECT, VMAD_STATUS_EDITED, &[object]),
            (
                "removedBool",
                VMAD_BOOL,
                VMAD_STATUS_REMOVED,
                std::slice::from_ref(&false_value),
            ),
            ("removedNone", VMAD_OBJECT, VMAD_STATUS_REMOVED, &[]),
            (
                "holeArray",
                VMAD_INT + VMAD_ARRAY_OFFSET,
                VMAD_STATUS_EDITED,
                &hole,
            ),
            ("oddStatus", VMAD_INT, 2, std::slice::from_ref(&zero)),
            (
                "damagedRemoved",
                VMAD_INT,
                VMAD_STATUS_REMOVED,
                std::slice::from_ref(&null),
            ),
            (
                "gapArray",
                VMAD_INT + VMAD_ARRAY_OFFSET,
                VMAD_STATUS_EDITED,
                &[zero.clone(), zero.clone(), zero.clone()],
            ),
        ];
        for (index, (name, kind, status, rows)) in cases.into_iter().enumerate() {
            add_property(
                &connection,
                i64::try_from(index).unwrap(),
                name,
                kind,
                status,
                rows,
            );
        }
        // `gapArray` is the last case: move its rows to the indexes 0, 2 and 3.
        connection
            .execute_batch(
                "UPDATE script_property_values SET value_index = 3 \
                 WHERE property_index = 13 AND value_index = 2; \
                 UPDATE script_property_values SET value_index = 2 \
                 WHERE property_index = 13 AND value_index = 1;",
            )
            .unwrap();
        drop(connection);
        let forms = SqliteForms::open(&dir.path().join("skyrim_world.db")).unwrap();
        let properties = forms.script_properties(REFERENCE, "S");
        let by_name = |name: &str| properties.iter().find(|p| p.name == name).unwrap();
        let unreadable = |name: &str| {
            assert!(
                matches!(by_name(name).status, PropertyStatus::Unreadable(_)),
                "{name}: {:?}",
                by_name(name)
            );
            assert_eq!(by_name(name).value, PropertyValue::None, "{name}");
        };
        for name in [
            "nullInt",
            "nullString",
            "nullObject",
            "nullAlias",
            "holeArray",
            "oddStatus",
            "gapArray",
        ] {
            unreadable(name);
        }
        assert_eq!(
            by_name("emptyString"),
            &ScriptProperty::edited("emptyString", PropertyValue::Str(String::new()))
        );
        assert_eq!(
            by_name("zeroInt"),
            &ScriptProperty::edited("zeroInt", PropertyValue::Int(0))
        );
        assert_eq!(
            by_name("falseBool"),
            &ScriptProperty::edited("falseBool", PropertyValue::Bool(false))
        );
        assert_eq!(
            by_name("object"),
            &ScriptProperty::edited(
                "object",
                PropertyValue::Object {
                    form_id: 0x2000,
                    alias: Some(0xFFFF)
                }
            )
        );
        assert_eq!(by_name("removedBool").status, REMOVED);
        // An Object property with a count of 0 has no value row: damaged, and reported.
        assert!(matches!(
            &by_name("removedNone").status,
            PropertyStatus::Removed { fault: Some(_) }
        ));
        assert!(matches!(
            &by_name("damagedRemoved").status,
            PropertyStatus::Removed { fault: Some(fault) } if fault.contains("NULL")
        ));
        assert_eq!(properties.len(), 14, "no row is dropped: {properties:?}");
    }

    #[test]
    fn fuzz_the_sqlite_reader_against_the_decoder() {
        let mut rng = Lcg(0x1052);
        let dir = TempDir::new().unwrap();
        let connection = property_database(dir.path());
        let mut expected = Vec::new();
        for index in 0..400_i64 {
            let scalar = 1 + i64::try_from(rng.below(5)).unwrap();
            let array = rng.below(2) == 0;
            let kind = if array {
                scalar + VMAD_ARRAY_OFFSET
            } else {
                scalar
            };
            let status = [1, 1, 1, 3, 0, 2][usize::try_from(rng.below(6)).unwrap()];
            let count = if array { rng.below(4) } else { 1 };
            let rows: Vec<ValueRow> = (0..count).map(|_| random_row(&mut rng)).collect();
            let name = format!("p{index}");
            add_property(&connection, index, &name, kind, status, &rows);
            expected.push(decoded(kind, status, i64::try_from(count).unwrap(), rows));
            expected.last_mut().unwrap().name = name;
        }
        drop(connection);
        let forms = SqliteForms::open(&dir.path().join("skyrim_world.db")).unwrap();
        assert_eq!(forms.script_properties(REFERENCE, "S"), expected);
    }

    /// The seeded value of one property of the `defaultsDemo` script.
    fn level_of(host: &PapyrusHost) -> i64 {
        host.object(REFERENCE).unwrap().get("::level_var").unwrap()
    }

    #[test]
    fn a_removed_property_leaves_the_script_default() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "defaultsDemo",
            vec![
                ScriptProperty {
                    name: "level".into(),
                    value: PropertyValue::Int(0),
                    status: REMOVED,
                },
                ScriptProperty {
                    name: "flag".into(),
                    value: PropertyValue::Bool(false),
                    status: REMOVED,
                },
            ],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        assert_eq!(level_of(&host), 42, "the default stays");
        assert!(
            host.object(REFERENCE)
                .unwrap()
                .get::<bool>("::flag_var")
                .unwrap()
        );
        assert_eq!(errors(&host), Vec::<String>::new());
    }

    #[test]
    fn a_removed_reference_override_keeps_the_base_value() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        forms.bases.insert(REFERENCE, 0x9000);
        attach(
            &mut forms,
            0x9000,
            "defaultsDemo",
            vec![ScriptProperty::edited("level", PropertyValue::Int(5))],
        );
        attach(
            &mut forms,
            REFERENCE,
            "defaultsDemo",
            vec![ScriptProperty {
                name: "level".into(),
                value: PropertyValue::Int(0),
                status: REMOVED,
            }],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        assert_eq!(
            level_of(&host),
            5,
            "the removed row does not clobber the base"
        );
    }

    #[test]
    fn an_edited_reference_override_still_wins_over_the_base() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        forms.bases.insert(REFERENCE, 0x9000);
        attach(
            &mut forms,
            0x9000,
            "defaultsDemo",
            vec![ScriptProperty::edited("level", PropertyValue::Int(5))],
        );
        attach(
            &mut forms,
            REFERENCE,
            "defaultsDemo",
            vec![ScriptProperty::edited("level", PropertyValue::Int(0))],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        assert_eq!(level_of(&host), 0, "a stored zero is a value");
    }

    #[test]
    fn an_unreadable_property_is_logged_and_sets_nothing() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        forms.bases.insert(REFERENCE, 0x9000);
        attach(
            &mut forms,
            0x9000,
            "defaultsDemo",
            vec![ScriptProperty::edited("level", PropertyValue::Int(5))],
        );
        attach(
            &mut forms,
            REFERENCE,
            "defaultsDemo",
            vec![ScriptProperty::unreadable("level", "integer_value is NULL")],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        assert_eq!(
            level_of(&host),
            5,
            "neither zero nor the default replaces the base"
        );
        let logged = errors(&host);
        assert_eq!(logged.len(), 1, "{logged:?}");
        assert!(logged[0].contains("level") && logged[0].contains("integer_value is NULL"));
    }

    fn alias_prop(name: &str, quest: u32, alias: u16) -> ScriptProperty {
        ScriptProperty::edited(
            name,
            PropertyValue::Object {
                form_id: quest,
                alias: Some(alias),
            },
        )
    }

    fn get_reference(host: &PapyrusHost, alias_object: &Table) -> Value {
        let call: Function = host.runtime().get("call_method").unwrap();
        let args = host.lua().create_table().unwrap();
        call.call((
            host.runtime().clone(),
            alias_object.clone(),
            "GetReference",
            args,
        ))
        .unwrap()
    }

    fn warnings(host: &PapyrusHost) -> Vec<String> {
        host.log_lines()
            .into_iter()
            .filter(|line| line.level == "warn")
            .map(|line| line.message)
            .collect()
    }

    #[test]
    fn an_alias_bound_property_holds_the_alias_and_reads_the_current_fill() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            // Alias id 0 is a real alias. Only 0xFFFF means "not bound".
            vec![alias_prop("Hero", QUEST, 0), alias_prop("Boss", QUEST, 3)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        let hero: Table = object.get("::Hero_var").unwrap();
        let boss: Table = object.get("::Boss_var").unwrap();
        assert_eq!(hero.get::<u32>("__form_id").unwrap(), QUEST);
        assert_eq!(hero.get::<i64>("__alias_id").unwrap(), 0);
        assert_eq!(boss.get::<i64>("__alias_id").unwrap(), 3);
        assert_eq!(hero.get::<String>("__name").unwrap(), "ReferenceAlias");
        // Empty alias: GetReference gives None.
        assert!(matches!(get_reference(&host, &boss), Value::Nil));
        // The quest fills the alias later. The same property object now reads the new target.
        host.world().aliases.insert((QUEST, 3), MARKER);
        let Value::Table(target) = get_reference(&host, &boss) else {
            panic!("a filled alias must give its reference");
        };
        assert_eq!(target.get::<u32>("__form_id").unwrap(), MARKER);
        assert!(
            matches!(get_reference(&host, &hero), Value::Nil),
            "alias 0 is still empty"
        );
        assert_eq!(warnings(&host), Vec::<String>::new());
        assert_eq!(errors(&host), Vec::<String>::new());
    }

    #[test]
    fn two_properties_bound_to_one_alias_share_one_object() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("Hero", QUEST, 7), alias_prop("Boss", QUEST, 7)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        let hero: Table = object.get("::Hero_var").unwrap();
        let boss: Table = object.get("::Boss_var").unwrap();
        assert!(
            hero.equals(&boss).unwrap(),
            "Papyrus `==` on one alias is true"
        );
    }

    #[test]
    fn an_alias_typed_and_a_reference_alias_typed_property_share_one_object() {
        // Both attach orders: the shared object always ends with the subtype's class.
        for order in [["Anyone", "Hero"], ["Hero", "Anyone"]] {
            let dir = fixture_dir();
            let mut forms = base_forms();
            attach(
                &mut forms,
                REFERENCE,
                "aliasUser",
                order.map(|name| alias_prop(name, QUEST, 5)).to_vec(),
            );
            let mut host = host_with(dir.path(), forms, true);
            host.attach_reference(REFERENCE);
            let object = host.object(REFERENCE).unwrap();
            let anyone: Table = object.get("::Anyone_var").unwrap();
            let hero: Table = object.get("::Hero_var").unwrap();
            assert!(anyone.equals(&hero).unwrap(), "{order:?}");
            assert_eq!(hero.get::<String>("__name").unwrap(), "ReferenceAlias");
            host.world().aliases.insert((QUEST, 5), MARKER);
            assert!(matches!(get_reference(&host, &hero), Value::Table(_)));
            assert_eq!(warnings(&host), Vec::<String>::new(), "{order:?}");
        }
    }

    #[test]
    fn one_alias_declared_as_two_unrelated_types_gives_the_second_property_none() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("Hero", QUEST, 6), alias_prop("Place", QUEST, 6)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        assert!(object.get::<Option<Table>>("::Hero_var").unwrap().is_some());
        assert!(
            object
                .get::<Option<Table>>("::Place_var")
                .unwrap()
                .is_none()
        );
        let logged = warnings(&host);
        assert!(
            logged
                .iter()
                .any(|m| m.contains("Place") && m.contains("unrelated alias type")),
            "{logged:?}"
        );
    }

    #[test]
    fn an_alias_script_type_stays_none_instead_of_an_object_with_empty_variables() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("Scripted", QUEST, 2)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        assert!(
            object
                .get::<Option<Table>>("::Scripted_var")
                .unwrap()
                .is_none()
        );
        let logged = warnings(&host);
        assert!(
            logged
                .iter()
                .any(|m| m.contains("Scripted") && m.contains("not attached yet")),
            "{logged:?}"
        );
    }

    #[test]
    fn a_replaced_alias_binding_does_not_fix_the_class_of_that_alias() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        forms.bases.insert(REFERENCE, 0x9000);
        attach(
            &mut forms,
            0x9000,
            "aliasUser",
            vec![alias_prop("Hero", QUEST, 5)],
        );
        // The reference moves `Hero` to alias 7, so alias 5 is free to be a location.
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("hero", QUEST, 7), alias_prop("Place", QUEST, 5)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        let hero: Table = object.get("::Hero_var").unwrap();
        assert_eq!(hero.get::<i64>("__alias_id").unwrap(), 7);
        let place: Table = object.get("::Place_var").unwrap();
        assert_eq!(place.get::<String>("__name").unwrap(), "LocationAlias");
        assert_eq!(warnings(&host), Vec::<String>::new());
    }

    #[test]
    fn a_removed_or_unreadable_override_leaves_the_base_alias_binding() {
        for status in [REMOVED, PropertyStatus::Unreadable("damaged".into())] {
            let dir = fixture_dir();
            let mut forms = base_forms();
            forms.bases.insert(REFERENCE, 0x9000);
            attach(
                &mut forms,
                0x9000,
                "aliasUser",
                vec![alias_prop("Hero", QUEST, 5)],
            );
            attach(
                &mut forms,
                REFERENCE,
                "aliasUser",
                vec![ScriptProperty {
                    status,
                    ..alias_prop("Hero", QUEST, 7)
                }],
            );
            let mut host = host_with(dir.path(), forms, true);
            host.attach_reference(REFERENCE);
            let hero: Table = host.object(REFERENCE).unwrap().get("::Hero_var").unwrap();
            assert_eq!(hero.get::<i64>("__alias_id").unwrap(), 5);
        }
    }

    #[test]
    fn an_alias_class_with_its_own_instance_field_is_refused() {
        // `alias_object` changes an object's class by its metatable only. A class that sets a
        // field in `new` would leave that field out, so the host refuses it, in both orders.
        for (class_name, order) in [
            ("referencealias", ["Anyone", "Hero"]),
            ("referencealias", ["Hero", "Anyone"]),
            ("alias", ["Anyone", "Hero"]),
        ] {
            let dir = fixture_dir();
            let path = dir.path().join(format!("{class_name}.luau"));
            let source = std::fs::read_to_string(&path).unwrap().replace(
                "return self\nend",
                "self[\"::count_var\"] = 0\n    return self\nend",
            );
            std::fs::write(&path, source).unwrap();
            let mut forms = base_forms();
            attach(
                &mut forms,
                REFERENCE,
                "aliasUser",
                order.map(|name| alias_prop(name, QUEST, 5)).to_vec(),
            );
            let mut host = host_with(dir.path(), forms, true);
            host.attach_reference(REFERENCE);
            let logged = errors(&host);
            assert!(
                logged
                    .iter()
                    .any(|m| m.contains("sets the instance field ::count_var")),
                "{class_name} {order:?}: {logged:?}"
            );
        }
    }

    #[test]
    fn a_damaged_removed_property_is_logged_and_sets_nothing() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "defaultsDemo",
            vec![ScriptProperty {
                name: "level".into(),
                value: PropertyValue::None,
                status: PropertyStatus::Removed {
                    fault: Some("integer_value is NULL".into()),
                },
            }],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        assert_eq!(level_of(&host), 42);
        let logged = errors(&host);
        assert_eq!(logged.len(), 1, "{logged:?}");
        assert!(logged[0].contains("removed property level") && logged[0].contains("NULL"));
    }

    #[test]
    fn unreadable_script_rows_are_logged_and_the_readable_ones_still_attach() {
        let dir = fixture_dir();
        let connection = property_database(dir.path());
        connection
            .execute_batch(&format!(
                "CREATE TABLE records(form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL); \
                 CREATE TABLE \"references\"(id INTEGER PRIMARY KEY, base_form_id INTEGER, \
                 header_flags INTEGER); \
                 INSERT INTO scripts VALUES ({REFERENCE}, x'00ff', x'00'); \
                 INSERT INTO scripts VALUES ('not a form', 'S', x'00'); \
                 INSERT INTO scripts VALUES ({REFERENCE}, 'defaultsDemo', x'00');"
            ))
            .unwrap();
        drop(connection);
        let forms = SqliteForms::open(&dir.path().join("skyrim_world.db")).unwrap();
        let mut host = host_with_source(dir.path(), Rc::new(forms));
        let report = host.attach_reference(REFERENCE);
        assert!(
            report.attached.contains(&"defaultsDemo".to_string()),
            "{report:?}"
        );
        let logged = errors(&host);
        assert!(
            logged.iter().any(|m| m.contains("unreadable form_id")),
            "{logged:?}"
        );
        assert!(
            logged
                .iter()
                .any(|m| m.contains("a script name of 00001000 cannot be read")),
            "{logged:?}"
        );
    }

    #[test]
    fn an_array_of_alias_bound_objects_holds_one_alias_object_per_element() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![ScriptProperty::edited(
                "Guards",
                PropertyValue::Array(vec![
                    PropertyValue::Object {
                        form_id: QUEST,
                        alias: Some(1),
                    },
                    PropertyValue::Object {
                        form_id: QUEST,
                        alias: Some(2),
                    },
                ]),
            )],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let guards: Table = host.object(REFERENCE).unwrap().get("::Guards_var").unwrap();
        assert_eq!(guards.raw_len(), 2);
        let first: Table = guards.get(1).unwrap();
        let second: Table = guards.get(2).unwrap();
        assert_eq!(first.get::<i64>("__alias_id").unwrap(), 1);
        assert_eq!(second.get::<i64>("__alias_id").unwrap(), 2);
        host.world().aliases.insert((QUEST, 2), MARKER);
        assert!(matches!(get_reference(&host, &first), Value::Nil));
        assert!(matches!(get_reference(&host, &second), Value::Table(_)));
        assert_eq!(warnings(&host), Vec::<String>::new());
    }

    #[test]
    fn an_unbound_object_property_still_resolves_to_the_form_not_an_alias() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("Plain", MARKER, u16::MAX)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let plain: Table = host.object(REFERENCE).unwrap().get("::Plain_var").unwrap();
        assert_eq!(plain.get::<u32>("__form_id").unwrap(), MARKER);
        assert!(plain.get::<Option<i64>>("__alias_id").unwrap().is_none());
    }

    #[test]
    fn an_alias_binding_without_a_retail_backed_type_stays_none_and_says_why() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            // `Plain` is declared ObjectReference. `Untyped` has no backing variable type.
            vec![
                alias_prop("Plain", QUEST, 4),
                alias_prop("Untyped", QUEST, 4),
            ],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        assert!(
            object
                .get::<Option<Table>>("::Plain_var")
                .unwrap()
                .is_none()
        );
        assert!(
            object
                .get::<Option<Table>>("::Untyped_var")
                .unwrap()
                .is_none()
        );
        let logged = warnings(&host);
        assert!(
            logged
                .iter()
                .any(|m| m.contains("Plain") && m.contains("not an alias type")),
            "{logged:?}"
        );
        assert!(
            logged
                .iter()
                .any(|m| m.contains("Untyped") && m.contains("declares no type")),
            "{logged:?}"
        );
    }

    #[test]
    fn an_alias_on_a_form_that_is_not_a_quest_stays_none() {
        let dir = fixture_dir();
        let mut forms = base_forms();
        attach(
            &mut forms,
            REFERENCE,
            "aliasUser",
            vec![alias_prop("Hero", MARKER, 1)],
        );
        let mut host = host_with(dir.path(), forms, true);
        host.attach_reference(REFERENCE);
        let object = host.object(REFERENCE).unwrap();
        assert!(object.get::<Option<Table>>("::Hero_var").unwrap().is_none());
        assert!(warnings(&host).iter().any(|m| m.contains("not a quest")));
    }
}
