use mlua::{Error, Function, Lua, Result, Table, VmState};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const SOURCE: &str = include_str!("../../shared/src/papyrus_runtime.luau");

/// Instruction budget for one entry into script code (#645). The counter belongs to the
/// whole VM, so the host calls [`ExecutionBudget::reset`] before every event, update and
/// resumed wait. A runaway call fails; a long session of short calls never does.
#[derive(Clone, Debug)]
pub struct ExecutionBudget {
    remaining: Arc<AtomicU64>,
    /// Times the budget ran out since the host last refilled it.
    strikes: Arc<AtomicU64>,
    per_call: u64,
}

/// After a call runs out, the scheduler that catches the error needs a few interrupts of its
/// own. The VM grants this much, at most [`MAX_GRACE_REFILLS`] times per host refill, so a
/// script that catches the error and loops again still stops.
const GRACE_INTERRUPTS: u64 = 5_000;
const MAX_GRACE_REFILLS: u64 = 8;

impl ExecutionBudget {
    pub fn per_call(&self) -> u64 {
        self.per_call
    }

    /// Refill the counter to the per-call allowance.
    pub fn reset(&self) {
        self.strikes.store(0, Ordering::Relaxed);
        self.remaining.store(self.per_call, Ordering::Relaxed);
    }

    /// Interrupts left before the current call fails.
    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Relaxed)
    }
}

/// Production defaults: the whole VM may hold 64 MiB, and one script call may use 2,000,000
/// interrupts before it fails. They are named here so tests and the engine do not scatter
/// the numbers.
pub const DEFAULT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
pub const DEFAULT_CALL_BUDGET: u64 = 2_000_000;

/// A sandboxed VM and the handle that refills its per-call budget.
pub fn new_sandboxed_vm(
    memory_limit: usize,
    per_call_budget: u64,
) -> Result<(Lua, ExecutionBudget)> {
    let lua = Lua::new();
    lua.set_memory_limit(memory_limit)?;
    lua.sandbox(true)?;
    let budget = ExecutionBudget {
        remaining: Arc::new(AtomicU64::new(per_call_budget)),
        strikes: Arc::new(AtomicU64::new(0)),
        per_call: per_call_budget,
    };
    let remaining = budget.remaining.clone();
    let strikes = budget.strikes.clone();
    lua.set_interrupt(move |_| {
        let exhausted = remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_sub(1)
            })
            .is_err();
        if exhausted {
            if strikes.fetch_add(1, Ordering::Relaxed) < MAX_GRACE_REFILLS {
                remaining.store(GRACE_INTERRUPTS.min(per_call_budget), Ordering::Relaxed);
            }
            Err(Error::RuntimeError(
                "Papyrus script execution budget exceeded".into(),
            ))
        } else {
            Ok(VmState::Continue)
        }
    });
    Ok((lua, budget))
}

/// A sandboxed VM whose budget the caller never refills. Kept for the small tests that run
/// one call; the engine uses [`new_sandboxed_vm`].
pub fn new_sandboxed_lua(memory_limit: usize, interrupt_budget: u64) -> Result<Lua> {
    new_sandboxed_vm(memory_limit, interrupt_budget).map(|(lua, _)| lua)
}

pub fn load_runtime(lua: &Lua) -> Result<Table> {
    let module: Table = lua.load(SOURCE).set_name("@papyrus_runtime.luau").eval()?;
    module.get::<Function>("new")?.call(())
}

/// Registers a class through the shared case-insensitive Papyrus name resolver.
pub fn register_class(runtime: &Table, name: &str, class: Table) -> Result<()> {
    let register: Function = runtime.get("register_class")?;
    register.call((runtime.clone(), name, class))
}

/// Registers native names with the same case-insensitive policy as emitted calls.
pub fn register_native(
    runtime: &Table,
    class_name: &str,
    function_name: &str,
    callback: Function,
) -> Result<()> {
    let register: Function = runtime.get("register_native")?;
    register.call((runtime.clone(), class_name, function_name, callback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_executes_calls_properties_arrays_and_natives() {
        let lua = new_sandboxed_lua(8 * 1024 * 1024, 10_000).unwrap();
        let runtime = load_runtime(&lua).unwrap();
        let class: Table = lua
            .load(
                r#"return {
                    __name = "Test",
                    Add = function(self, a, b) return a + b end,
                    get_Value = function(self) return self.value end,
                    set_Value = function(self, value) self.value = value end,
                }"#,
            )
            .eval()
            .unwrap();
        register_class(&runtime, "Test", class.clone()).unwrap();
        let call_method: Function = runtime.get("call_method").unwrap();
        let args = lua.create_sequence_from([2, 3]).unwrap();
        let sum: i64 = call_method
            .call((runtime.clone(), class.clone(), "Add", args))
            .unwrap();
        assert_eq!(sum, 5);

        let find: Function = runtime.get("array_find").unwrap();
        let values = lua.create_sequence_from([10, 20, 30]).unwrap();
        let index: i64 = find.call((runtime.clone(), values, 20, 0)).unwrap();
        assert_eq!(index, 1);

        let cast: Function = runtime.get("cast").unwrap();
        let int_val: i64 = cast.call((runtime.clone(), true, "int")).unwrap();
        assert_eq!(int_val, 1);
        let float_val: f64 = cast.call((runtime.clone(), true, "float")).unwrap();
        assert_eq!(float_val, 1.0);

        let native = lua
            .create_function(|_, (_target, value): (Table, i64)| Ok(value * 2))
            .unwrap();
        register_native(&runtime, "Test", "Double", native).unwrap();
        let invoke: Function = runtime.get("native").unwrap();
        let args = lua.create_sequence_from([21]).unwrap();
        let doubled: i64 = invoke.call((runtime, class, "Double", args)).unwrap();
        assert_eq!(doubled, 42);
    }

    /// Rust registrations retain host tables and resolve names independently of their spelling.
    #[test]
    fn host_registration_resolves_mixed_case_names() {
        let lua = new_sandboxed_lua(8 * 1024 * 1024, 10_000).unwrap();
        let runtime = load_runtime(&lua).unwrap();
        let class: Table = lua
            .load(
                r#"return {
            __name = 'HostClass', __parent = '', __autoState = '', Flag = false,
            ['Busy::Value'] = function(self) return 47 end,
            Value = function(self) return 11 end,
            get_Counter = function(self) return self.counter end,
            set_Counter = function(self, value) self.counter = value end,
        }"#,
            )
            .eval()
            .unwrap();
        register_class(&runtime, "HOSTCLASS", class.clone()).unwrap();
        let callback = lua
            .create_function(|_, (_target, value): (Table, i64)| Ok(value + 3))
            .unwrap();
        register_native(&runtime, "hostclass", "DOUBLE", callback).unwrap();
        lua.globals().set("rt", runtime).unwrap();
        lua.globals().set("Class", class).unwrap();
        lua.load(
            r#"
            assert(rt:resolve_identifier(nil, 'hOsTcLaSs') == Class)
            local a = setmetatable({}, { __index = Class })
            rt:call_method(a, 'gOtOsTaTe', {'bUsY'})
            assert(rt:call_method(a, 'gEtStAtE', {}) == 'bUsY')
            assert(rt:call_method(a, 'vAlUe', {}) == 47)
            assert(rt:native(a, 'dOuBlE', {70}) == 73)
            assert(rt:call_static('hostclass', 'VALUE', {}) == 11)
            rt:set_property(a, 'COUNTER', 89)
            assert(rt:get_property(a, 'counter') == 89)
            rt:set_property(a, 'FLAG', true)
            assert(a.Flag == true and a.FLAG == nil and Class.Flag == false)
            assert(Class['Busy::Value'] ~= nil and Class.Value ~= nil, 'host keys retained')
        "#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn sandbox_stops_unbounded_scripts() {
        let lua = new_sandboxed_lua(2 * 1024 * 1024, 10).unwrap();
        let error = lua.load("while true do end").exec().unwrap_err();
        assert!(error.to_string().contains("budget exceeded"));
    }

    /// Two calls that each use 60% of the allowance both succeed once the host refills the
    /// budget between them. A single runaway call still fails. Without the refill the second
    /// call fails, which is what a long session did before the budget reset (#645).
    #[test]
    fn the_budget_refills_for_each_call_and_still_stops_a_runaway() {
        let work = "return function(n) local x = 0 for i = 1, n do x = x + i end return x end";
        // Measure what a fixed amount of work costs.
        let (lua, budget) = new_sandboxed_vm(2 * 1024 * 1024, 1_000_000).unwrap();
        let function: Function = lua.load(work).eval().unwrap();
        function.call::<i64>(10_000).unwrap();
        let cost = budget.per_call() - budget.remaining();
        assert!(cost > 100, "the interrupt counted {cost} for the work");

        // An allowance of cost / 0.6: each call uses 60%.
        let (lua, budget) = new_sandboxed_vm(2 * 1024 * 1024, cost * 10 / 6).unwrap();
        let function: Function = lua.load(work).eval().unwrap();
        function.call::<i64>(10_000).unwrap();
        budget.reset();
        function.call::<i64>(10_000).unwrap();
        budget.reset();
        let runaway = function.call::<i64>(1_000_000).unwrap_err();
        assert!(runaway.to_string().contains("budget exceeded"));
        // The failed call left no debt: after a refill the VM runs normally again.
        budget.reset();
        function.call::<i64>(10_000).unwrap();

        // The same two calls with no refill: the second one fails.
        let (lua, _budget) = new_sandboxed_vm(2 * 1024 * 1024, cost * 10 / 6).unwrap();
        let function: Function = lua.load(work).eval().unwrap();
        function.call::<i64>(10_000).unwrap();
        let second = function.call::<i64>(10_000).unwrap_err();
        assert!(second.to_string().contains("budget exceeded"));
    }
}
