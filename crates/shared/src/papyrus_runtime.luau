local Runtime = {}
Runtime.__index = Runtime

function Runtime.new()
    return setmetatable({ classes = {}, natives = {} }, Runtime)
end

function Runtime:register_class(name, class)
    self.classes[name] = class
end

function Runtime:register_native(class_name, function_name, callback)
    self.natives[class_name .. "::" .. function_name] = callback
end

function Runtime:resolve_identifier(_target, name)
    return self.classes[name]
end

function Runtime:native(target, name, args)
    local class_name = target.__name or ""
    local callback = self.natives[class_name .. "::" .. name]
    assert(callback, "unregistered Papyrus native: " .. class_name .. "::" .. name)
    return callback(target, table.unpack(args))
end

-- Search each script's current state, then its empty state, before its parent.
-- The receiver remains the original instance when executing inherited code.
function Runtime:find_method(class, state, name)
    local seen = {}
    while class ~= nil do
        assert(not seen[class], "cyclic Papyrus parent hierarchy")
        seen[class] = true
        local callback = state ~= "" and class[state .. "::" .. name] or nil
        if callback == nil then callback = class[name] end
        if callback ~= nil then return callback end
        class = self.classes[class.__parent]
    end
    return nil
end

function Runtime:call_method(target, name, args)
    assert(target ~= nil, "Papyrus method target is None")
    if name == "GotoState" then
        self:goto_state(target, args[1] or "")
        return nil
    elseif name == "GetState" then
        return target.__state or target.__autoState or ""
    end
    local state = target.__state or target.__autoState or ""
    local callback = self:find_method(target, state, name)
    if callback == nil and target.dispatch ~= nil then
        return target:dispatch(name, table.unpack(args))
    end
    assert(type(callback) == "function", "unknown Papyrus method: " .. (target.__name or "") .. "::" .. name)
    return callback(target, table.unpack(args))
end

-- Emitted Parent calls supply the lexical parent's name, so a parent method's
-- own Parent call can advance through the hierarchy on a derived receiver.
function Runtime:call_parent(target, name, args, parent_name)
    local parent = parent_name or target.__parent
    local class = self.classes[parent]
    assert(class, "unknown Papyrus parent class: " .. tostring(parent))
    local state = target.__state or target.__autoState or ""
    local callback = self:find_method(class, state, name)
    assert(type(callback) == "function", "unknown parent method: " .. name)
    return callback(target, table.unpack(args))
end

function Runtime:call_static(class_name, name, args)
    local class = self.classes[class_name]
    assert(class, "unknown Papyrus class: " .. tostring(class_name))
    local callback = class[name]
    assert(type(callback) == "function", "unknown static method: " .. name)
    return callback(class, table.unpack(args))
end

function Runtime:get_property(target, name)
    assert(target ~= nil, "Papyrus property target is None")
    local getter = target["get_" .. name]
    if type(getter) == "function" then
        return getter(target)
    end
    return target[name]
end

function Runtime:set_property(target, name, value)
    assert(target ~= nil, "Papyrus property target is None")
    local setter = target["set_" .. name]
    if type(setter) == "function" then
        setter(target, value)
    else
        target[name] = value
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

-- Unknown destinations are diagnosed before any script side effects. This is
-- Mudcrab policy; Skyrim's CK documentation also describes undefined states.
function Runtime:goto_state(target, state)
    assert(target ~= nil, "Papyrus state target is None")
    assert(type(state) == "string", "Papyrus state name must be a string")
    local class, seen = target, {}
    local declared = state == ""
    while class ~= nil and not declared do
        assert(not seen[class], "cyclic Papyrus parent hierarchy")
        seen[class] = true
        declared = class.__states ~= nil and class.__states[state] == true
        class = self.classes[class.__parent]
    end
    assert(declared, "unknown Papyrus state: " .. (target.__name or "") .. "::" .. state)
    local old_state = target.__state or target.__autoState or ""
    local on_end = self:find_method(target, old_state, "OnEndState")
    if on_end ~= nil then on_end(target) end
    target.__state = state
    local on_begin = self:find_method(target, state, "OnBeginState")
    if on_begin ~= nil then on_begin(target) end
end

return Runtime
