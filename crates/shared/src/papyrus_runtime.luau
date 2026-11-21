local Runtime = {}
Runtime.__index = Runtime

-- Host hooks, all optional and set by the embedding engine:
--   loader(lowercase_name) -> class table or nil   loads a converted script on demand
--   logger(level, message)                          receives every runtime log line
--   reset_budget()                                  called before each script thread resumes
function Runtime.new()
    local self = setmetatable({
        classes = {},
        natives = {},
        missing = {},
        objects = {},
        proxies = {},
        statics = {},
        messages = {},
        unbound = {},
        notes = {},
        fault_counts = {},
        sleepers = {},
        updates = {},
        sequence = 0,
        clock = 0,
        strict_natives = false,
    }, Runtime)
    self:register_builtin_natives()
    return self
end

-- Every runtime fault lands here. It never raises: a Papyrus script fault logs and the
-- script continues with None, so one bad script cannot take the engine down (#630).
function Runtime:log(level, message)
    local messages = self.messages
    messages[#messages + 1] = { level = level, message = message }
    if #messages > 256 then table.remove(messages, 1) end
    local logger = self.logger
    if logger ~= nil then logger(level, message) end
end

-- A note that only matters the first time, for example "this native is a partial stub".
function Runtime:note_once(key, message)
    if self.notes[key] then return end
    self.notes[key] = true
    self:log("warn", message)
end

-- Log a message at most once per distinct text, and count the repeats in `fault_counts`.
-- A script that faults on every `OnUpdate` tick would otherwise write one line per tick.
function Runtime:log_once(level, message)
    local count = (self.fault_counts[message] or 0) + 1
    self.fault_counts[message] = count
    if count == 1 then self:log(level, message) end
    return count
end

function Runtime:fault(message)
    self:log_once("error", message)
    return nil
end

-- Resolve a class by Papyrus name, loading it through the host on first use.
-- Misses are remembered so a missing script does not hit the disk on every call.
function Runtime:class_named(name)
    if type(name) ~= "string" or name == "" then return nil end
    local lowered = string.lower(name)
    local class = self.classes[lowered]
    if class ~= nil then return class end
    if self.missing[lowered] then return nil end
    local loader = self.loader
    if loader ~= nil then
        class = loader(lowered)
        if class ~= nil then
            self.classes[lowered] = class
            return class
        end
    end
    self.missing[lowered] = true
    return nil
end

-- Papyrus identifiers use ASCII case-insensitive lookup; data strings retain spelling.
-- Keep an exact-key fast path and inspect original keys on a case mismatch. This
-- also supports host class tables without rewriting their fields or callbacks.
function Runtime:identifier_key(target, name)
    if rawget(target, name) ~= nil then return name end
    local normalized = string.lower(name)
    for key in pairs(target) do
        if type(key) == "string" and string.lower(key) == normalized then return key end
    end
    return nil
end

function Runtime:get_field(target, name)
    local key = self:identifier_key(target, name)
    if key ~= nil then return rawget(target, key) end
    local meta = getmetatable(target)
    local index = meta and meta.__index
    if type(index) == "table" then
        key = self:identifier_key(index, name)
        if key ~= nil then return rawget(index, key) end
    end
    return nil
end

function Runtime:set_field(target, name, value)
    -- Type metadata retains declared field names even when their value is None.
    local key = self:identifier_key(target, name)
    local types = target.__types
    if key == nil and types ~= nil then key = self:identifier_key(types, name) end
    if key == nil then
        local meta = getmetatable(target)
        local index = meta and meta.__index
        if type(index) == "table" then key = self:identifier_key(index, name) end
    end
    target[key or name] = value
end

function Runtime:register_class(name, class)
    self.classes[string.lower(name)] = class
end

function Runtime:register_native(class_name, function_name, callback)
    self.natives[string.lower(class_name .. "::" .. function_name)] = callback
end

function Runtime:resolve_identifier(_target, name)
    return self:class_named(name)
end

-- Natives are registered under the class that declares them. An instance of a child
-- script carries the child's name, so walk up the parent chain until a class has one
-- (#629). A native nobody bound is counted and logged; it never raises unless the host
-- turned on `strict_natives` for a test.
function Runtime:native(target, name, args)
    if target == nil then
        return self:fault("native " .. tostring(name) .. " called on None")
    end
    local lowered = string.lower(name)
    local class_name, parent_name = target.__name, target.__parent
    for _ = 1, 64 do
        if type(class_name) == "string" and class_name ~= "" then
            local callback = self.natives[string.lower(class_name) .. "::" .. lowered]
            if callback ~= nil then return callback(target, table.unpack(args)) end
        end
        local class = self:class_named(parent_name)
        if class == nil then break end
        class_name, parent_name = class.__name, class.__parent
    end
    return self:unbound_native(target, name)
end

function Runtime:declaring_class_name(target, name)
    local class_name = target.__name
    for _ = 1, 64 do
        local class = self:class_named(class_name)
        if class == nil then break end
        if rawget(class, name) ~= nil or self:identifier_key(class, name) ~= nil then
            return class.__name or class_name
        end
        class_name = class.__parent
    end
    return target.__name or "?"
end

function Runtime:unbound_native(target, name)
    local key = self:declaring_class_name(target, name) .. "::" .. name
    local count = (self.unbound[key] or 0) + 1
    self.unbound[key] = count
    local message = "unbound Papyrus native " .. key .. " (called on " .. tostring(target.__name) .. ")"
    if self.strict_natives then error(message, 2) end
    -- First call logs; later calls only count, so a hot script cannot flood the log.
    if count == 1 then self:log("error", message .. "; returning None") end
    return nil
end

-- Search the active state through the ancestry before trying empty-state fallback.
-- The receiver remains the original instance when executing inherited code.
function Runtime:find_method(class, state, name)
    local cyclic = false
    for pass = 1, state ~= "" and 2 or 1 do
        local key = pass == 1 and state ~= "" and state .. "::" .. name or name
        local current = class
        local seen = {}
        while current ~= nil do
            if seen[current] then
                cyclic = true
                break
            end
            seen[current] = true
            local callback = self:get_field(current, key)
            if callback ~= nil then return callback end
            current = self:class_named(current.__parent)
        end
    end
    assert(not cyclic, "cyclic Papyrus parent hierarchy")
    return nil
end

function Runtime:call_method(target, name, args)
    if target == nil then
        return self:fault("call to " .. tostring(name) .. " on None")
    end
    local normalized = string.lower(name)
    if normalized == "gotostate" then
        self:goto_state(target, args[1] or "")
        return nil
    elseif normalized == "getstate" then
        return target.__state or target.__autoState or ""
    end
    local state = target.__state or target.__autoState or ""
    local callback = self:find_method(target, state, name)
    if callback == nil and target.dispatch ~= nil then
        return target:dispatch(name, table.unpack(args))
    end
    if type(callback) ~= "function" then
        return self:fault("unknown Papyrus method: " .. tostring(target.__name) .. "::" .. name)
    end
    return callback(target, table.unpack(args))
end

-- Emitted Parent calls supply the lexical parent's name, so a parent method's
-- own Parent call can advance through the hierarchy on a derived receiver.
function Runtime:call_parent(target, name, args, parent_name)
    local parent = parent_name or target.__parent
    local class = self:class_named(parent)
    if class == nil then
        return self:fault("unknown Papyrus parent class: " .. tostring(parent))
    end
    local state = target.__state or target.__autoState or ""
    local callback = self:find_method(class, state, name)
    if type(callback) ~= "function" then
        return self:fault("unknown parent method: " .. tostring(parent) .. "::" .. name)
    end
    return callback(target, table.unpack(args))
end

function Runtime:call_static(class_name, name, args)
    local class = self:class_named(class_name)
    if class == nil then
        return self:fault("unknown Papyrus class: " .. tostring(class_name))
    end
    local callback = self:get_field(class, name)
    if type(callback) ~= "function" then
        return self:fault("unknown static method: " .. tostring(class_name) .. "::" .. name)
    end
    -- Global emitted functions need this runtime too. Use a per-call context so
    -- classes shared by multiple runtime instances keep their registration data.
    local context = setmetatable({ __runtime = self }, { __index = class })
    return callback(context, table.unpack(args))
end

function Runtime:get_property(target, name)
    if target == nil then
        return self:fault("read of property " .. tostring(name) .. " on None")
    end
    local getter = self:find_method(target, "", "get_" .. name)
    if type(getter) == "function" then
        return getter(target)
    end
    return self:get_field(target, name)
end

function Runtime:set_property(target, name, value)
    if target == nil then
        return self:fault("write of property " .. tostring(name) .. " on None")
    end
    local setter = self:find_method(target, "", "set_" .. name)
    if type(setter) == "function" then
        setter(target, value)
    else
        self:set_field(target, name, value)
    end
end

function Runtime:cast(value, type_name)
    local normalized = string.lower(type_name or "")
    if normalized == "int" or normalized == "integer" then
        if type(value) == "boolean" then
            return value and 1 or 0
        end
        return math.floor(tonumber(value) or 0)
    elseif normalized == "float" then
        if type(value) == "boolean" then
            return value and 1.0 or 0.0
        end
        return tonumber(value) or 0.0
    elseif normalized == "bool" or normalized == "boolean" then
        return not not value
    elseif normalized == "string" then
        return value == nil and "" or tostring(value)
    end
    return value
end

function Runtime:array_find(array, value, start_index)
    local first = math.max(0, start_index or 0)
    for index = first + 1, #array do
        if array[index] == value then return index - 1 end
    end
    return -1
end

function Runtime:array_rfind(array, value, start_index)
    local first = start_index
    if first == nil or first < 0 or first >= #array then first = #array - 1 end
    for index = first + 1, 1, -1 do
        if array[index] == value then return index - 1 end
    end
    return -1
end

-- Undeclared destinations are valid: preserve the supplied name and resolve
-- lifecycle handlers and subsequent calls with the usual empty-state fallback.
function Runtime:goto_state(target, state)
    assert(target ~= nil, "Papyrus state target is None")
    assert(type(state) == "string", "Papyrus state name must be a string")
    local old_state = target.__state or target.__autoState or ""
    local on_end = self:find_method(target, old_state, "OnEndState")
    if on_end ~= nil then on_end(target) end
    target.__state = state
    local on_begin = self:find_method(target, state, "OnBeginState")
    if on_begin ~= nil then on_begin(target) end
end

-- Script objects ---------------------------------------------------------------------

-- Create one script object for a form. The object keeps its form id in `__form_id`,
-- which natives read to find the form in the host's world.
function Runtime:instantiate(class_name, form_id)
    local class = self:class_named(class_name)
    if class == nil then
        return self:fault("cannot attach script " .. tostring(class_name) .. ": class not found")
    end
    local object
    if type(class.new) == "function" then
        object = class.new(self)
    else
        object = setmetatable({ __runtime = self }, { __index = class })
    end
    rawset(object, "__form_id", form_id)
    local list = self.objects[form_id]
    if list == nil then
        list = {}
        self.objects[form_id] = list
    end
    list[#list + 1] = object
    return object
end

-- Whether the class (or an ancestor) has this name.
function Runtime:inherits(class_name, ancestor)
    local wanted = string.lower(ancestor)
    local name = class_name
    for _ = 1, 64 do
        if type(name) ~= "string" or name == "" then return false end
        if string.lower(name) == wanted then return true end
        local class = self:class_named(name)
        if class == nil then return false end
        name = class.__parent
    end
    return false
end

-- The object other scripts see for a form: its first attached script object, or a plain
-- typed proxy. Natives such as GetLinkedRef return these.
function Runtime:proxy(form_id, class_name)
    if form_id == nil or form_id == 0 then return nil end
    local list = self.objects[form_id]
    if list ~= nil and list[1] ~= nil then return list[1] end
    local proxy = self.proxies[form_id]
    if proxy ~= nil then return proxy end
    local class = self:class_named(class_name) or self:class_named("Form")
    if class == nil then
        return self:fault("cannot make a proxy for form " .. tostring(form_id) .. ": class " .. tostring(class_name) .. " not found")
    end
    if type(class.new) == "function" then
        proxy = class.new(self)
    else
        proxy = setmetatable({ __runtime = self }, { __index = class })
    end
    rawset(proxy, "__form_id", form_id)
    self.proxies[form_id] = proxy
    return proxy
end

-- Scheduler --------------------------------------------------------------------------
-- Every entry into script code (an event, an update) runs in its own coroutine so that
-- Utility.Wait can suspend it. A script fault ends only that thread and is logged with
-- the label of the call that started it. The host calls `advance` once per frame.

function Runtime:spawn(label, fn)
    local thread = coroutine.create(fn)
    self:resume(thread, label)
end

function Runtime:resume(thread, label)
    if self.reset_budget ~= nil then self.reset_budget() end
    local ok, request = coroutine.resume(thread)
    if not ok then
        self:log_once("error", label .. ": " .. tostring(request))
        return
    end
    if coroutine.status(thread) == "suspended" then
        local seconds = type(request) == "table" and tonumber(request.wait) or 0
        self.sequence = self.sequence + 1
        self.sleepers[#self.sleepers + 1] = {
            thread = thread,
            label = label,
            wake = self.clock + math.max(0, seconds),
            sequence = self.sequence,
        }
    end
end

local function by_time_then_order(a, b)
    local left = a.wake or a.next
    local right = b.wake or b.next
    if left ~= right then return left < right end
    return a.sequence < b.sequence
end

function Runtime:advance(seconds)
    self.clock = self.clock + (seconds or 0)
    local due, waiting = {}, {}
    for _, entry in ipairs(self.sleepers) do
        if entry.wake <= self.clock then
            due[#due + 1] = entry
        else
            waiting[#waiting + 1] = entry
        end
    end
    self.sleepers = waiting
    table.sort(due, by_time_then_order)
    for _, entry in ipairs(due) do
        self:resume(entry.thread, entry.label)
    end
    local fired = {}
    for _, registration in pairs(self.updates) do
        if registration.next <= self.clock then fired[#fired + 1] = registration end
    end
    table.sort(fired, by_time_then_order)
    for _, registration in ipairs(fired) do
        if registration.single then
            self.updates[registration.target] = nil
        else
            -- Keep the cadence (no drift), but never queue more than one missed tick.
            registration.next = registration.next + registration.interval
            if registration.next <= self.clock then
                registration.next = self.clock + registration.interval
            end
        end
        self:send_event(registration.target, "OnUpdate")
    end
end

-- Dispatch an event to one script object. A missing handler is normal Papyrus and is
-- skipped without a log line. Returns whether a handler ran.
function Runtime:send_event(target, event, ...)
    if target == nil then return false end
    local state = target.__state or target.__autoState or ""
    local handler = self:find_method(target, state, event)
    if type(handler) ~= "function" then return false end
    local args = table.pack(...)
    local label = tostring(target.__name) .. "::" .. event
    self:spawn(label, function()
        return handler(target, table.unpack(args, 1, args.n))
    end)
    return true
end

-- Utility.Wait suspends the calling script thread. Outside a thread it cannot, so it
-- logs and returns at once instead of silently doing nothing.
function Runtime:wait(seconds)
    if not coroutine.isyieldable() then
        return self:fault("Utility.Wait called outside a script thread; the wait was skipped")
    end
    coroutine.yield({ wait = tonumber(seconds) or 0 })
    return nil
end

function Runtime:register_update(target, interval, single)
    if target == nil then return self:fault("RegisterForUpdate on None") end
    interval = tonumber(interval) or 0
    if interval <= 0 then
        return self:fault("RegisterForUpdate needs a positive interval, got " .. tostring(interval))
    end
    self.sequence = self.sequence + 1
    self.updates[target] = {
        target = target,
        interval = interval,
        next = self.clock + interval,
        single = single,
        sequence = self.sequence,
    }
    return nil
end

-- Natives the runtime owns because they only touch the scheduler or the log.
function Runtime:register_builtin_natives()
    local runtime = self
    local function level_for(severity)
        if severity == 2 then return "error" elseif severity == 1 then return "warn" end
        return "info"
    end
    self:register_native("Utility", "Wait", function(_, seconds) return runtime:wait(seconds) end)
    self:register_native("Utility", "WaitMenuMode", function(_, seconds) return runtime:wait(seconds) end)
    -- Form, Alias and ActiveMagicEffect each declare their own update natives.
    for _, class_name in ipairs({ "Form", "Alias", "ActiveMagicEffect" }) do
        self:register_native(class_name, "RegisterForUpdate", function(target, interval)
            return runtime:register_update(target, interval, false)
        end)
        self:register_native(class_name, "RegisterForSingleUpdate", function(target, interval)
            return runtime:register_update(target, interval, true)
        end)
        self:register_native(class_name, "UnregisterForUpdate", function(target)
            runtime.updates[target] = nil
            return nil
        end)
    end
    self:register_native("Debug", "Trace", function(_, text, severity)
        runtime:log(level_for(severity or 0), "Papyrus trace: " .. tostring(text))
        return nil
    end)
    self:register_native("Debug", "Notification", function(_, text)
        runtime:log("info", "Papyrus notification: " .. tostring(text))
        return nil
    end)
end

return Runtime
