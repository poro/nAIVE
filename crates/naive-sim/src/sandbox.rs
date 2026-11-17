//! The Lua sandbox shared by the game's script VM and the headless test VM.
//!
//! # What the CPU limit covers
//!
//! The instruction hook interrupts Lua bytecode, including metamethods,
//! comparators and replacement functions that library code calls back into.
//! It cannot interrupt a C library function once it has started. So every
//! library function a script can reach is one of:
//!
//! - constant time (`math`, `type`, `rawget`, `string.len`, ...);
//! - charged to the same budget *before* it runs, at one unit per element it
//!   visits or per 16 bytes it copies (per 4 bytes it decodes or compiles),
//!   so a call that would exceed the budget is refused instead of started;
//! - reimplemented: pattern matching (`find`, `match`, `gmatch`, `gsub`) runs
//!   in Rust and charges each matcher step, and inserting or removing inside a
//!   table with a metatable shifts elements in Lua.
//!
//! Charges smaller than 16 units are not sent to the budget; the wrapper's
//! own instructions, which the hook does count, cost about as much.
//!
//! What is *not* metered: some work inside the VM and library internals that
//! no wrapper can see in advance. Examples are `..` on long strings, the bytes
//! `table.concat` assembles (charged per element), `next` over a table whose
//! entries were removed (it scans the free slots), and a `utf8.codes`
//! iterator called by hand on a long run of continuation bytes. Each such
//! operation is bounded by the memory limit, but a loop can repeat it. In the
//! game VM a wall-clock limit per call (`ScriptLimits::wall_time_per_call`)
//! is the backstop. It is checked whenever the budget is charged, so a call
//! stops at most one hook interval (10,000 instructions) of such operations
//! after its time is up. The test VM has its own deadline per test and runs
//! under process supervision.
//!
//! Errors keep stock Lua's messages and point at the script's line, with
//! these known differences:
//! - A wrapped C function is named by its qualified name (`'table.insert'`).
//! - Method-call argument numbers are not shifted.
//! - An error without a location raised inside a callback (a sort
//!   comparator, a `__tostring`) gains the calling line.
//! - A script error whose message is exactly `"not enough memory"` raised
//!   inside such a callback counts as the memory limit.
//!
//! The sandbox also omits `io`, `os`, `package`, `debug` and `coroutine`
//! (the hook only covers the main thread), removes `loadfile`, `dofile` and
//! `string.dump`, loads only text chunks, and refuses `__gc` finalizers:
//! Lua runs finalizers with hooks disabled, so one could loop forever.

mod patterns;
#[cfg(test)]
mod tests;

use std::rc::Rc;

use mlua::prelude::*;
use mlua::{LuaOptions, StdLib};

use patterns::{CaptureValue, Found, MatchError, Meter};

/// Charges `units` of work to the caller's instruction budget, failing with
/// the limit error once it is exhausted.
pub type Charge = Rc<dyn Fn(&Lua, u64) -> LuaResult<()>>;

/// Options that differ between the game and test VMs.
#[derive(Clone, Copy)]
pub struct SandboxOptions {
    /// Memory the Lua state may hold, in bytes.
    pub memory_bytes: usize,
    /// Whether scripts may compile code at run time with `load` (text only).
    pub allow_load: bool,
}

/// Create a sandboxed Lua state whose library work is charged to `charge`.
/// The caller installs its instruction hook, which charges the same budget.
pub fn create_lua(options: SandboxOptions, charge: Charge) -> LuaResult<Lua> {
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_memory_limit(options.memory_bytes)?;

    let globals = lua.globals();
    globals.set("loadfile", LuaNil)?;
    globals.set("dofile", LuaNil)?;
    if !options.allow_load {
        globals.set("load", LuaNil)?;
    }

    let charge_units = {
        let charge = charge.clone();
        lua.create_function(move |lua, units: f64| {
            // Fractions and negative estimates round to nothing; beyond u64 is
            // over any budget anyway.
            if units >= 1.0 {
                charge(lua, units.min(u64::MAX as f64) as u64)?;
            }
            Ok(())
        })?
    };
    let error_marker = lua.create_table()?;
    let raw = create_pattern_functions(&lua, &charge, &error_marker)?;

    let originals: LuaTable = lua.load(PRELUDE).set_name("=[naive sandbox]").call((
        charge_units,
        options.memory_bytes as f64,
        error_marker,
        raw,
    ))?;
    // Lua names a tail-called library function in an argument error by
    // finding it in the loaded-module table, so file each original there under
    // its library name. Scripts have no `package` or `debug` to reach it.
    let loaded: LuaTable = lua.named_registry_value("_LOADED")?;
    for pair in originals.pairs::<LuaString, LuaFunction>() {
        let (name, function) = pair?;
        loaded.raw_set(name, function)?;
    }
    Ok(lua)
}

fn type_name(value: &LuaValue) -> &'static str {
    match value {
        LuaValue::Nil => "nil",
        LuaValue::Boolean(_) => "boolean",
        LuaValue::LightUserData(_) | LuaValue::UserData(_) => "userdata",
        LuaValue::Integer(_) | LuaValue::Number(_) => "number",
        LuaValue::String(_) => "string",
        LuaValue::Table(_) => "table",
        LuaValue::Function(_) => "function",
        LuaValue::Thread(_) => "thread",
        _ => "userdata",
    }
}

/// An argument error raised as a plain string, like `luaL_argerror`.
fn arg_error(position: usize, function: &str, message: impl std::fmt::Display) -> MatchError {
    MatchError::Pattern(format!(
        "bad argument #{position} to '{function}' ({message})"
    ))
}

fn check_string(
    lua: &Lua,
    args: &[LuaValue],
    position: usize,
    function: &str,
) -> Result<LuaString, MatchError> {
    let value = args.get(position - 1).cloned().unwrap_or(LuaNil);
    let name = type_name(&value);
    match lua.coerce_string(value)? {
        Some(string) => Ok(string),
        None => Err(arg_error(
            position,
            function,
            format!(
                "string expected, got {}",
                name_for_missing(args, position, name)
            ),
        )),
    }
}

/// `luaL_typeerror` says "no value" for a missing argument.
fn name_for_missing(args: &[LuaValue], position: usize, name: &'static str) -> &'static str {
    if position > args.len() {
        "no value"
    } else {
        name
    }
}

fn opt_integer(
    lua: &Lua,
    args: &[LuaValue],
    position: usize,
    function: &str,
    default: i64,
) -> Result<i64, MatchError> {
    let value = args.get(position - 1).cloned().unwrap_or(LuaNil);
    if value.is_nil() {
        return Ok(default);
    }
    let name = type_name(&value);
    let is_number = matches!(value, LuaValue::Integer(_) | LuaValue::Number(_));
    let numeric_string = match &value {
        LuaValue::String(s) => lua.coerce_number(LuaValue::String(s.clone()))?.is_some(),
        _ => false,
    };
    match lua.coerce_integer(value)? {
        Some(integer) => Ok(integer),
        None if is_number || numeric_string => Err(arg_error(
            position,
            function,
            "number has no integer representation",
        )),
        None => Err(arg_error(
            position,
            function,
            format!("number expected, got {name}"),
        )),
    }
}

fn push_captures(
    lua: &Lua,
    out: &mut Vec<LuaValue>,
    captures: Vec<CaptureValue<'_>>,
) -> LuaResult<()> {
    for capture in captures {
        out.push(match capture {
            CaptureValue::Bytes(bytes) => LuaValue::String(lua.create_string(bytes)?),
            CaptureValue::Position(position) => LuaValue::Integer(position as i64),
        });
    }
    Ok(())
}

/// Turn a matcher result into Lua return values. Pattern and argument
/// errors come back as `(marker, message)` so the Lua wrapper can raise them
/// as plain strings; budget errors propagate as errors.
fn finish(
    lua: &Lua,
    error_marker: &LuaTable,
    result: Result<Vec<LuaValue>, MatchError>,
) -> LuaResult<LuaMultiValue> {
    match result {
        Ok(values) => Ok(LuaMultiValue::from_iter(values)),
        Err(MatchError::Pattern(message)) => Ok(LuaMultiValue::from_iter([
            LuaValue::Table(error_marker.clone()),
            LuaValue::String(lua.create_string(message)?),
        ])),
        Err(MatchError::Lua(error)) => Err(error),
    }
}

/// One Rust half of a pattern function: arguments in, Lua results out.
type PatternBody = fn(&Lua, &mut Meter<'_>, &[LuaValue]) -> Result<Vec<LuaValue>, MatchError>;

/// The Rust halves of `string.find`, `match`, `gmatch` and `gsub`, in a table
/// the prelude consumes and does not expose.
fn create_pattern_functions(
    lua: &Lua,
    charge: &Charge,
    error_marker: &LuaTable,
) -> LuaResult<LuaTable> {
    let functions = lua.create_table()?;

    let make = |lua: &Lua, body: PatternBody| -> LuaResult<LuaFunction> {
        let charge = charge.clone();
        let marker = error_marker.clone();
        lua.create_function(move |lua, args: LuaMultiValue| {
            let charge_lua = |units: u64| charge(lua, units);
            let mut meter = Meter::new(&charge_lua);
            let args: Vec<LuaValue> = args.into_iter().collect();
            finish(lua, &marker, body(lua, &mut meter, &args))
        })
    };

    functions.set(
        "find",
        make(lua, |lua, meter, args| {
            find_or_match(lua, meter, args, true)
        })?,
    )?;
    functions.set(
        "match",
        make(lua, |lua, meter, args| {
            find_or_match(lua, meter, args, false)
        })?,
    )?;
    functions.set("gsub", make(lua, gsub)?)?;
    functions.set("gmatch_start", make(lua, gmatch_start)?)?;
    functions.set("next", make(lua, next)?)?;
    // Re-raise a caught allocation failure as one, so limits still see it.
    functions.set(
        "raise_memory",
        lua.create_function(|_, ()| -> LuaResult<()> {
            Err(LuaError::MemoryError("not enough memory".into()))
        })?,
    )?;
    Ok(functions)
}

fn find_or_match(
    lua: &Lua,
    meter: &mut Meter<'_>,
    args: &[LuaValue],
    is_find: bool,
) -> Result<Vec<LuaValue>, MatchError> {
    let name = if is_find { "find" } else { "match" };
    let subject = check_string(lua, args, 1, name)?;
    let pattern = check_string(lua, args, 2, name)?;
    let subject = subject.as_bytes();
    let pattern = pattern.as_bytes();
    let init = patterns::start_position(opt_integer(lua, args, 3, name, 1)?, subject.len()) - 1;
    if init > subject.len() {
        return Ok(vec![LuaNil]);
    }
    let plain = is_find
        && args
            .get(3)
            .is_some_and(|value| !matches!(value, LuaValue::Nil | LuaValue::Boolean(false)));
    let mut out = Vec::new();
    match patterns::find(meter, &subject, &pattern, init, plain, is_find)? {
        None => out.push(LuaNil),
        Some(Found::Span(start, end, captures)) => {
            out.push(LuaValue::Integer(start as i64));
            out.push(LuaValue::Integer(end as i64));
            push_captures(lua, &mut out, captures)?;
        }
        Some(Found::Captures(captures)) => push_captures(lua, &mut out, captures)?,
    }
    Ok(out)
}

/// `gsub(s, pattern, repl, n)`. A string or number replacement is done here.
/// For a function or table replacement this validates the arguments and
/// returns `false, s, pattern, n` for the Lua loop, which calls back into
/// script code with ordinary Lua error semantics.
fn gsub(lua: &Lua, meter: &mut Meter<'_>, args: &[LuaValue]) -> Result<Vec<LuaValue>, MatchError> {
    let subject = check_string(lua, args, 1, "gsub")?;
    let pattern = check_string(lua, args, 2, "gsub")?;
    // Lua reads the count (argument 4) before checking the replacement.
    let subject_len = subject.as_bytes().len() as i64;
    let max = opt_integer(lua, args, 4, "gsub", subject_len + 1)?;
    let replacement = args.get(2).cloned().unwrap_or(LuaNil);
    let dynamic = match &replacement {
        LuaValue::String(_) | LuaValue::Integer(_) | LuaValue::Number(_) => false,
        LuaValue::Function(_) | LuaValue::Table(_) => true,
        other => {
            return Err(arg_error(
                3,
                "gsub",
                format!(
                    "string/function/table expected, got {}",
                    name_for_missing(args, 3, type_name(other))
                ),
            ))
        }
    };
    if dynamic {
        return Ok(vec![
            LuaValue::Boolean(false),
            LuaValue::String(subject),
            LuaValue::String(pattern),
            LuaValue::Integer(max),
        ]);
    }
    let replacement = lua
        .coerce_string(replacement)?
        .ok_or_else(|| MatchError::Lua(LuaError::runtime("unreachable gsub replacement")))?;
    let (result, count) = patterns::gsub_string(
        meter,
        &subject.as_bytes(),
        &pattern.as_bytes(),
        &replacement.as_bytes(),
        max,
    )?;
    let first = match result {
        Some(bytes) => LuaValue::String(lua.create_string(bytes)?),
        // Unchanged: the subject, which C has already converted to a string.
        None => LuaValue::String(subject.clone()),
    };
    Ok(vec![
        LuaValue::Boolean(true),
        first,
        LuaValue::Integer(count),
    ])
}

/// Validate `gmatch(s, pattern, init)` and return `s, pattern, start` with a
/// 0-based start.
fn gmatch_start(
    lua: &Lua,
    _meter: &mut Meter<'_>,
    args: &[LuaValue],
) -> Result<Vec<LuaValue>, MatchError> {
    let subject = check_string(lua, args, 1, "gmatch")?;
    let pattern = check_string(lua, args, 2, "gmatch")?;
    let len = subject.as_bytes().len();
    let init = patterns::start_position(opt_integer(lua, args, 3, "gmatch", 1)?, len) - 1;
    let init = init.min(len + 1);
    Ok(vec![
        LuaValue::String(subject),
        LuaValue::String(pattern),
        LuaValue::Integer(init as i64),
    ])
}

/// `next(s, pattern, from, last_end, anchor)` with 0-based positions and
/// `last_end` of -1 for none: `start, end, captures...` or nil.
fn next(lua: &Lua, meter: &mut Meter<'_>, args: &[LuaValue]) -> Result<Vec<LuaValue>, MatchError> {
    let (Some(LuaValue::String(subject)), Some(LuaValue::String(pattern))) =
        (args.first(), args.get(1))
    else {
        return Err(MatchError::Lua(LuaError::runtime(
            "sandbox: bad pattern state",
        )));
    };
    let integer = |i: usize| match args.get(i) {
        Some(LuaValue::Integer(n)) => *n,
        _ => -1,
    };
    let from = integer(2).max(0) as usize;
    let last_end = usize::try_from(integer(3)).ok();
    let anchor = matches!(args.get(4), Some(LuaValue::Boolean(true)));
    let subject = subject.as_bytes();
    let pattern = pattern.as_bytes();
    let mut out = Vec::new();
    match patterns::next_match(meter, &subject, &pattern, from, last_end, anchor)? {
        None => out.push(LuaNil),
        Some((start, end, captures)) => {
            out.push(LuaValue::Integer(start as i64));
            out.push(LuaValue::Integer(end as i64));
            push_captures(lua, &mut out, captures)?;
        }
    }
    Ok(out)
}

/// Installs the metered library. Runs once per state with the engine's
/// arguments; originals live only in its upvalues.
const PRELUDE: &str = r#"
local charge, memory_limit, ERR, raw = ...
local type, select, rawlen, rawget, getmetatable, error =
    type, select, rawlen, rawget, getmetatable, error
local tointeger, mtype, ult = math.tointeger, math.type, math.ult

-- Raise a pattern/argument error from the Rust half as a plain string at
-- the script's call site, as luaL_error does. `checked` is tail-called by the
-- wrappers, so level 2 is the script; `checked_inner` is called from inside a
-- wrapper, one level deeper.
local function checked(first, ...)
    if first == ERR then error((...), 2) end
    return first, ...
end
local function checked_inner(first, ...)
    if first == ERR then error((...), 3) end
    return first, ...
end
local function size(value)
    local t = type(value)
    if t == 'string' then return #value end
    if t == 'number' then return 24 end
    return 0
end
-- Small charges are not sent to the budget: the wrapper's own instructions,
-- which the hook counts, cost about as much. This keeps short-string calls
-- cheap.
local function bill(units)
    if units >= 16 then charge(units) end
end
local function copy_units(bytes) return bytes / 16 + 1 end
local function scan_units(bytes) return bytes / 4 + 1 end
-- String slice bounds as str_sub/str_byte compute them (posrelatI, getendpos).
local function slice(len, i, j)
    i = tointeger(i) or 1
    j = tointeger(j)
    if j == nil then j = -1 end
    if i > 0 then elseif i == 0 or i < -len then i = 1 else i = len + i + 1 end
    if j > len then j = len elseif j >= 0 then elseif j < -len then j = 0 else j = len + j + 1 end
    if i > j then return 0 end
    return j - i + 1
end

-- Wrappers call the original C function through `relay(pcall(f, ...))`.
-- Under pcall a C library error carries no location, so relay raises it at
-- the script's line, as a direct call would; errors that already have a
-- location (from a callback the function ran) pass through unchanged, and
-- memory errors stay memory errors. The originals go back to Rust, which
-- files them in the registry's loaded-module table (unreachable from
-- scripts), so argument errors can name them ('table.insert').
local originals = {}
local function original(name, f)
    originals[name] = f
    return f
end
local pcall, raise_memory = pcall, raw.raise_memory
local function relay(ok, ...)
    if ok then return ... end
    local message = ...
    if message == 'not enough memory' then return raise_memory() end
    if type(message) == 'string' and not message:find('^[^\n]-:%d+: ') then
        error(message, 2)
    end
    error(message, 0)
end

-- Finalizers run with hooks disabled, so a looping __gc would escape the
-- CPU limit. Lua marks an object for finalization only when its metatable is
-- set, so checking the raw field here also covers a __gc added later.
do
    local setmetatable = original('setmetatable', setmetatable)
    _G.setmetatable = function(value, metatable)
        if type(metatable) == 'table' and rawget(metatable, '__gc') ~= nil then
            error('scripts do not support __gc finalizers', 2)
        end
        return relay(pcall(setmetatable, value, metatable))
    end
end

do
    local collectgarbage = original('collectgarbage', collectgarbage)
    _G.collectgarbage = function(option, ...)
        -- Collections and mode switches (which collect) visit the whole heap.
        if option ~= 'count' and option ~= 'isrunning' then
            charge(collectgarbage('count') * 16)
        end
        return relay(pcall(collectgarbage, option, ...))
    end
end

do
    local tonumber = original('tonumber', tonumber)
    _G.tonumber = function(...)
        local value = ...
        -- Converting a long string scans all of it.
        if type(value) == 'string' and #value > 64 then charge(scan_units(#value)) end
        if select('#', ...) == 1 then return tonumber(value) end
        return relay(pcall(tonumber, ...))
    end
end

if load then
    local load = original('load', load)
    _G.load = function(chunk, ...)
        local name, mode, env = ...
        -- Text only: a mode without 't' gets nothing.
        if type(mode) == 'string' and not mode:find('t', 1, true) then
            return nil, "attempt to load a text chunk (mode is '" .. mode .. "')"
        end
        if type(chunk) == 'string' then
            charge(scan_units(#chunk))
        elseif type(chunk) == 'function' then
            -- Compiling a reader's pieces is charged as they arrive.
            local reader = chunk
            chunk = function()
                local piece = reader()
                if type(piece) == 'string' then charge(scan_units(#piece)) end
                return piece
            end
        end
        if select('#', ...) >= 3 then
            return relay(pcall(load, chunk, name, 't', env))
        end
        return relay(pcall(load, chunk, name, 't'))
    end
end

string.dump = nil

-- Pattern functions: Rust matcher, metered per step.
do
    local find, match, gsub, gmatch_start, next_match =
        raw.find, raw.match, raw.gsub, raw.gmatch_start, raw.next
    local sub, byte, concat, typeof = string.sub, string.byte, table.concat, type
    string.find = function(...) return checked(find(...)) end
    string.match = function(...) return checked(match(...)) end

    string.gmatch = function(...)
        local s, p, src = checked_inner(gmatch_start(...))
        local last = -1
        local function step(start, e, ...)
            if start == ERR then error(e, 2) end
            if start == nil then return end
            src, last = e, e
            return ...
        end
        return function() return step(next_match(s, p, src, last, false)) end
    end

    string.gsub = function(...)
        local done, s, p, max = checked_inner(gsub(...))
        if done then return s, p end
        local repl = select(3, ...)
        local anchor = byte(p, 1) == 94
        if anchor then p = sub(p, 2) end
        local kind = typeof(repl)
        local parts, count, src, last, changed = {}, 0, 0, -1, false
        local n = 0
        local function apply(start, e, ...)
            if start == ERR then error(e, 3) end
            if start == nil then return false end
            if start > src then count = count + 1; parts[count] = sub(s, src + 1, start) end
            n = n + 1
            local value
            if kind == 'function' then value = repl(...) else value = repl[(...)] end
            if not value then
                count = count + 1; parts[count] = sub(s, start + 1, e)
            else
                local vt = typeof(value)
                if vt ~= 'string' and vt ~= 'number' then
                    error('invalid replacement value (a ' .. vt .. ')', 3)
                end
                count = count + 1; parts[count] = value
                changed = true
            end
            src, last = e, e
            return true
        end
        while n < max do
            if not apply(next_match(s, p, src, last, anchor)) then break end
            if anchor then break end
        end
        if not changed then return s, n end
        count = count + 1; parts[count] = sub(s, src + 1)
        return concat(parts), n
    end
end

-- Other string functions: charged before the C function runs.
do
    local rep = original('string.rep', string.rep)
    string.rep = function(s, n, sep)
        local times = tointeger(n)
        if times and times > 0 then
            local total = (size(s) + size(sep)) * (times + 0.0)
            -- Larger results fail allocation at once. The C loop runs `times`
            -- iterations even for empty strings.
            if total <= memory_limit then bill(copy_units(total) + times / 8) end
        end
        if type(s) == 'string' and times and (sep == nil or type(sep) == 'string') then
            return rep(s, times, sep)
        end
        return relay(pcall(rep, s, n, sep))
    end
    local function sized_args(...)
        local total = 0
        for i = 1, select('#', ...) do total = total + size((select(i, ...))) end
        return total
    end
    local format = original('string.format', string.format)
    string.format = function(fmt, ...)
        bill(copy_units(size(fmt) + sized_args(...)))
        return relay(pcall(format, fmt, ...))
    end
    local byte = original('string.byte', string.byte)
    string.byte = function(s, i, j)
        if type(s) == 'string' then
            bill(slice(#s, i, j == nil and (tointeger(i) or 1) or j) + 1)
            if (i == nil or mtype(i) == 'integer') and (j == nil or mtype(j) == 'integer') then
                return byte(s, i, j)
            end
        end
        return relay(pcall(byte, s, i, j))
    end
    local sub = original('string.sub', string.sub)
    string.sub = function(s, i, j)
        if type(s) == 'string' then
            bill(copy_units(slice(#s, i, j)))
            if mtype(i) == 'integer' and (j == nil or mtype(j) == 'integer') then
                return sub(s, i, j)
            end
        end
        return relay(pcall(sub, s, i, j))
    end
    local char = original('string.char', string.char)
    string.char = function(...)
        bill(select('#', ...) + 1)
        return relay(pcall(char, ...))
    end
    for _, name in ipairs({ 'upper', 'lower', 'reverse' }) do
        local f = original('string.' .. name, string[name])
        string[name] = function(s)
            if type(s) == 'string' then
                bill(copy_units(#s))
                return f(s)
            end
            return relay(pcall(f, s))
        end
    end
    local pack = original('string.pack', string.pack)
    local packsize = original('string.packsize', string.packsize)
    string.pack = function(fmt, ...)
        -- Fixed formats can produce far more than their inputs ('c100000').
        local fixed, output = pcall(packsize, fmt)
        bill(copy_units(size(fmt) + sized_args(...) + (fixed and output or 0)))
        return relay(pcall(pack, fmt, ...))
    end
    local unpack = original('string.unpack', string.unpack)
    string.unpack = function(fmt, ...)
        bill(copy_units(size(fmt)))
        -- Results go through relay twice on the stack; skip it for long
        -- formats, which can return many values.
        if size(fmt) > 10000 then return unpack(fmt, ...) end
        return relay(pcall(unpack, fmt, ...))
    end
    string.packsize = function(fmt)
        bill(copy_units(size(fmt)))
        return relay(pcall(packsize, fmt))
    end
end

-- Table functions. Element counts are computed once and passed explicitly
-- where the C function accepts them, so a __len metamethod cannot report a
-- small length to the charge and a large one to the C loop.
do
    local concat = original('table.concat', table.concat)
    table.concat = function(t, sep, i, j)
        if type(t) == 'table' then
            if j == nil then j = #t end
            local first, last = tointeger(i or 1), tointeger(j)
            if first and last and last >= first then bill((last + 0.0) - first + 1) end
        end
        return relay(pcall(concat, t, sep, i, j))
    end
    local unpack = original('table.unpack', table.unpack)
    table.unpack = function(t, i, j)
        if j == nil and (type(t) == 'table' or type(t) == 'string') then j = #t end
        local first, last = tointeger(i or 1), tointeger(j)
        -- Over a million results fail the stack check before any work.
        if first and last and last >= first and (last + 0.0) - first < 1e7 then
            bill((last + 0.0) - first + 1)
        end
        -- Valid arguments cannot raise an argument error; returning directly
        -- keeps a single copy of a large result list on the stack.
        if type(t) == 'table' and first and last then return unpack(t, first, last) end
        return relay(pcall(unpack, t, i, j))
    end
    local move = original('table.move', table.move)
    table.move = function(a1, f, e, t, a2)
        local first, last = tointeger(f), tointeger(e)
        if first and last and last >= first then bill((last + 0.0) - first + 1) end
        return relay(pcall(move, a1, f, e, t, a2))
    end
    local pack = original('table.pack', table.pack)
    table.pack = function(...)
        bill(select('#', ...) / 16 + 1)
        return relay(pcall(pack, ...))
    end

    local sort = original('table.sort', table.sort)
    local function sort_cost(n, t)
        local log = 1
        while (1 << log) < n do log = log + 1 end
        local factor = 1
        if type(t[1]) == 'string' then
            -- String comparisons cost their length.
            local longest = 0
            for k = 1, n do
                local v = rawget(t, k)
                if type(v) == 'string' and #v > longest then longest = #v end
            end
            factor = 1 + longest // 64
        end
        return (n + 0.0) * log * factor
    end
    table.sort = function(t, comp)
        if type(t) ~= 'table' then return relay(pcall(sort, t, comp)) end
        local mt = getmetatable(t)
        -- Any metatable may carry a __len (a __metatable field can hide it).
        if mt ~= nil then
            -- Sort a plain copy: C would read the length again itself.
            local n = #t
            local copy = {}
            for k = 1, n do copy[k] = t[k] end
            bill(sort_cost(n, copy))
            local ok, problem = pcall(sort, copy, comp)
            if not ok then return relay(false, problem) end
            for k = 1, n do t[k] = copy[k] end
            return
        end
        bill(sort_cost(rawlen(t), t))
        return relay(pcall(sort, t, comp))
    end

    local insert = original('table.insert', table.insert)
    local remove = original('table.remove', table.remove)
    table.insert = function(t, ...)
        local nargs = select('#', ...)
        if nargs == 1 and type(t) == 'table' and getmetatable(t) == nil then
            return insert(t, ...)
        end
        if nargs ~= 2 or type(t) ~= 'table' then return relay(pcall(insert, t, ...)) end
        local pos, value = ...
        local p = tointeger(pos)
        if getmetatable(t) == nil then
            local e = rawlen(t) + 1
            if p and p >= 1 and p <= e then bill(e - p + 1) end
            return relay(pcall(insert, t, pos, value))
        end
        -- Shift in Lua so every step is an instruction, reading the length
        -- once (a __len may answer differently each time).
        local n = #t
        if mtype(n) ~= 'integer' then error("object length is not an integer", 2) end
        local e = n + 1
        if not p then return relay(pcall(insert, t, pos, value)) end
        if not ult(p - 1, e) then
            error("bad argument #2 to 'insert' (position out of bounds)", 2)
        end
        for k = e, p + 1, -1 do t[k] = t[k - 1] end
        t[p] = value
    end
    table.remove = function(t, ...)
        if select('#', ...) == 0 and type(t) == 'table' and getmetatable(t) == nil then
            return remove(t)
        end
        if type(t) ~= 'table' or select('#', ...) == 0 then return relay(pcall(remove, t, ...)) end
        local pos = ...
        local p = tointeger(pos)
        if getmetatable(t) == nil then
            local n = rawlen(t)
            if p and p >= 1 and p <= n then bill(n - p + 1) end
            return relay(pcall(remove, t, pos))
        end
        -- As C does, but reading the length once (see insert).
        local size = #t
        if mtype(size) ~= 'integer' then error("object length is not an integer", 2) end
        if not p then return relay(pcall(remove, t, pos)) end
        if p ~= size and ult(size, p - 1) then
            error("bad argument #2 to 'remove' (position out of bounds)", 2)
        end
        local value = t[p]
        while p < size do
            t[p] = t[p + 1]
            p = p + 1
        end
        t[p] = nil
        return value
    end
end

do
    local char = original('utf8.char', utf8.char)
    local codepoint = original('utf8.codepoint', utf8.codepoint)
    local len = original('utf8.len', utf8.len)
    local offset = original('utf8.offset', utf8.offset)
    utf8.char = function(...)
        bill(select('#', ...) + 1)
        return relay(pcall(char, ...))
    end
    utf8.codepoint = function(s, i, j, lax)
        if type(s) == 'string' then
            local count = slice(#s, i, j == nil and (tointeger(i) or 1) or j)
            bill(scan_units(count))
            -- Many results: return them directly instead of through relay.
            if count > 10000 then return codepoint(s, i, j, lax) end
        end
        return relay(pcall(codepoint, s, i, j, lax))
    end
    utf8.len = function(s, ...)
        bill(scan_units(size(s)))
        return relay(pcall(len, s, ...))
    end
    utf8.offset = function(s, ...)
        bill(scan_units(size(s)))
        return relay(pcall(offset, s, ...))
    end
end

return originals
"#;
