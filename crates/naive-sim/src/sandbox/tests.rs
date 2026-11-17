use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::prelude::*;

use super::{create_lua, Charge, SandboxOptions};

/// A sandbox whose library charges and VM instructions count against
/// `limit`, as in the game and test VMs. `used` reports library charges only.
fn sandbox(limit: u64) -> (Lua, Rc<Cell<u64>>) {
    let used = Rc::new(Cell::new(0u64));
    let instructions = Rc::new(Cell::new(0u64));
    let charge: Charge = {
        let used = used.clone();
        let instructions = instructions.clone();
        Rc::new(move |_lua, units| {
            let total = used.get().saturating_add(units);
            used.set(total);
            if total.saturating_add(instructions.get()) > limit {
                Err(LuaError::runtime("budget exceeded"))
            } else {
                Ok(())
            }
        })
    };
    let options = SandboxOptions {
        memory_bytes: 64 * 1024 * 1024,
        allow_load: true,
    };
    let lua = create_lua(options, charge).unwrap();
    let library = used.clone();
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(1000),
        move |_lua, _debug| {
            instructions.set(instructions.get() + 1000);
            if instructions.get().saturating_add(library.get()) > limit {
                Err(LuaError::runtime("budget exceeded"))
            } else {
                Ok(mlua::VmState::Continue)
            }
        },
    )
    .unwrap();
    (lua, used)
}

/// Evaluates expressions and describes their results or error, identically
/// in the sandbox and in stock Lua.
const HARNESS: &str = r#"
return function(code)
    -- Not a tail call, so errors name the function as a script call would.
    local f = assert(load('local r = table.pack(' .. code .. ') return table.unpack(r, 1, r.n)', '=case'))
    local result = table.pack(pcall(f))
    local parts = {}
    for i = 2, result.n do
        local v = result[i]
        local kind = type(v)
        local shown = (kind == 'table' or kind == 'function') and kind or tostring(v)
        parts[#parts + 1] = (math.type(v) or type(v)) .. ':' .. shown
    end
    return (result[1] and 'ok ' or 'error ') .. table.concat(parts, ',')
end
"#;

fn lua_quote(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for b in bytes {
        out.push_str(&format!("\\{b:03}"));
    }
    out.push('"');
    out
}

fn expressions_for(subject: &[u8], pattern: &[u8]) -> Vec<String> {
    let s = lua_quote(subject);
    let p = lua_quote(pattern);
    vec![
        format!("string.find({s}, {p})"),
        format!("string.find({s}, {p}, 2)"),
        format!("string.find({s}, {p}, -3)"),
        format!("string.find({s}, {p}, 50)"),
        format!("string.find({s}, {p}, 1, true)"),
        format!("string.match({s}, {p})"),
        format!("string.match({s}, {p}, 3)"),
        format!(
            "(function() local t = {{}} for a, b in string.gmatch({s}, {p}) do \
             t[#t + 1] = tostring(a) .. '|' .. tostring(b) end return table.concat(t, ';') end)()"
        ),
        format!("string.gsub({s}, {p}, '<%0>')"),
        format!("string.gsub({s}, {p}, '%1%%')"),
        format!("string.gsub({s}, {p}, 'x', 1)"),
        format!("string.gsub({s}, {p}, 7)"),
        format!(
            "string.gsub({s}, {p}, function(a, b) if a == 'a' then return nil end \
             return tostring(a) .. tostring(b) end)"
        ),
        format!("string.gsub({s}, {p}, {{a = 'A', b = false, ab = 3}})"),
        format!("({s}):gsub({p}, '-', 2)"),
    ]
}

fn assert_same(cases: &[String]) {
    let (sandboxed, _) = sandbox(u64::MAX);
    let stock = Lua::new();
    let ours: LuaFunction = sandboxed.load(HARNESS).eval().unwrap();
    let theirs: LuaFunction = stock.load(HARNESS).eval().unwrap();
    for case in cases {
        let expected: String = theirs.call(case.as_str()).unwrap();
        let actual: String = ours.call(case.as_str()).unwrap();
        // Wrapped library functions are tail-called, so Lua names them by
        // their qualified name ('table.insert' rather than 'insert').
        let actual = ["string.", "table.", "utf8."]
            .iter()
            .fold(actual, |text, prefix| {
                text.replace(&format!("'{prefix}"), "'")
            });
        assert_eq!(actual, expected, "\n  case: {case}");
    }
}

#[test]
fn patterns_match_stock_lua_on_known_cases() {
    let pairs: &[(&str, &str)] = &[
        ("hello world", "o w"),
        ("hello world", "l+"),
        ("hello world", "(h)(e)"),
        ("hello", "()ll()"),
        ("  trim me  ", "^%s*(.-)%s*$"),
        ("key = value", "^(%w+)%s*=%s*(.-)$"),
        ("THE (quick) fox", "%((%a+)%)"),
        ("f(a(b)c)d", "%b()"),
        ("THE (quick) fox", "%f[%a]%a+"),
        ("hello", ""),
        ("", ""),
        ("", "x*"),
        ("abc", "$"),
        ("a$b", "a$b"),
        ("aaa", "a-b"),
        ("aaab", "a-b"),
        ("xyz", "[x-z]+"),
        ("x]y", "[]]"),
        ("a-b", "[a%-]+"),
        ("x^y", "[%^x]+"),
        ("abcabc", "(abc)%1"),
        ("hello", "[^l]+"),
        ("a.b", "%."),
        ("one two  three", "%s+"),
        ("tab\there", "%c"),
        ("x1y2", "%d"),
        ("A1!", "%p"),
        ("aB", "%u%l?"),
        ("ff0A", "%x+"),
        ("vt\u{b}x", "%s"),
        ("abc", "x*"),
        ("abc", "^a"),
        ("abc", "^b"),
        ("aaa", "^a-"),
        ("ab", "a?b?"),
        ("[a]", "[%]]"),
        ("a,b,,c", "([^,]*)"),
        ("hello", "(l)(l)"),
        ("hello", "((l)l)"),
        ("x", "[a-]"),
        ("-", "[a-]"),
        ("z", "[^%a]"),
        // Malformed patterns and invalid captures.
        ("abc", "[a"),
        ("abc", "%"),
        ("abc", "(()"),
        ("abc", "%b"),
        ("abc", "%ba"),
        ("abc", "%f"),
        ("abc", "%fa"),
        ("abc", "%1"),
        ("abc", "(a)%2"),
        ("abc", "a)"),
        ("abc", "%0"),
        ("abc", "(a)%1"),
        ("aa", "(a)%1"),
        ("abc", "()%1"),
        ("a\0b", "%z"),
        ("a\0b", "%Z+"),
        ("a\0b", "[%z]"),
    ];
    let mut cases = Vec::new();
    for (subject, pattern) in pairs {
        cases.extend(expressions_for(subject.as_bytes(), pattern.as_bytes()));
    }
    // Argument handling and replacement strings.
    cases.extend(
        [
            "string.find(12345, 34)",
            "string.find('abc', 'b', 1.0)",
            "string.find('abc', 'b', 1.5)",
            "string.find('abc', 'b', '2')",
            "string.find('abc', 'b', 'x')",
            "string.find(nil, 'b')",
            "string.find('abc')",
            "string.match('abc', {})",
            "string.gsub('abc', 'b')",
            "string.gsub('abc', 'b', true)",
            "string.gsub('a', 'a', true, 'x')",
            "select('#', table.unpack({}, 1, 600000))",
            "load('return 1', 'chunk', 'b')",
            "tonumber('0x10'), tonumber('10', 2), tonumber(' 12 '), tonumber('z', 36)",
            "pcall(tonumber, '10', 99)",
            "string.gsub('abc', 'b', '%2')",
            "string.gsub('abc', 'b', '%')",
            "string.gsub('abc', 'b', '%x')",
            "string.gsub('abc', '(b)', '%1%0%%')",
            "string.gsub('hello', '()ll', '%1')",
            "string.gsub('abc', 'b', function() return {} end)",
            "string.gsub('abc', 'b', function() return true end)",
            "string.gsub('abc', 'b', function() error('from callback', 0) end)",
            "string.gsub('abc', 'b', function() error({}) end)",
            "string.gsub(123, 'x', 'y')",
            "string.gsub('abc', '', '-')",
            "string.gsub('abc', '.', '%0%0', -1)",
            "string.gsub('abc', '.', '%0%0', 0)",
            "string.gsub('abc', '.', '%0%0', 2.0)",
            "string.gsub('abc', '^', '>')",
            "string.gsub('  x  ', '^%s+', '')",
            "string.gsub('abc', '%w', {a = 1, b = 2.5})",
            "string.gmatch('abc')",
            "string.gmatch('abc', '.', 10)",
            "string.gmatch('abc', '.', -1)()",
            "(function() local f = string.gmatch('a1b2', '%a(%d)') return f(), f(), f() end)()",
            "('x'):rep(3, ',')",
            "string.rep('ab', 0)",
            "string.rep('ab', -1)",
            "string.byte('hello', 2, 4)",
            "string.byte('hello', -1)",
            "string.byte('hello', 10)",
            "string.sub('hello', 2, -2)",
            "string.sub('hello', -100, 100)",
            "string.char(104, 105)",
            "string.upper('abc'), string.lower('ABC'), string.reverse('abc')",
            "string.format('%d %s %q', 5, 'x', 'y\\n')",
            "string.pack('i4', 7):len(), string.unpack('i4', string.pack('i4', 7))",
            "string.packsize('i4i8')",
            "table.concat({1, 2, 3}, ',')",
            "table.concat({1, 2, 3}, ',', 2)",
            "table.concat({1, 2, 3}, ',', 2, 5)",
            "table.concat({}, ',', 1, 0)",
            "table.unpack({1, 2, 3})",
            "table.unpack({1, 2, 3}, 2)",
            "table.unpack({1, 2, 3}, -1, 1)",
            "table.concat(table.move({1, 2, 3}, 1, 3, 2), ',')",
            "table.pack(1, nil, 3).n",
            "(function() local t = {5, 2, 8, 1} table.sort(t) return table.concat(t, ',') end)()",
            "(function() local t = {'b', 'a', 'c'} table.sort(t, function(a, b) return a > b end) \
             return table.concat(t, ',') end)()",
            "(function() local t = {1, 2, 3} table.insert(t, 1, 0) table.insert(t, 9) \
             return table.concat(t, ',') end)()",
            "(function() local t = {1, 2, 3} table.insert(t, 5, 0) return 1 end)()",
            "(function() local t = {1, 2, 3} return table.remove(t, 1), table.concat(t, ',') end)()",
            "(function() local t = {1, 2, 3} return table.remove(t), table.concat(t, ',') end)()",
            "(function() local t = {} return table.remove(t, 0), #t end)()",
            "(function() local t = {1, 2} return table.remove(t, 3), #t end)()",
            "(function() local t = {1, 2} return table.remove(t, 7) end)()",
            "table.insert({}, 1, 2, 3)",
            "table.insert(nil, 1)",
            "utf8.char(72, 228, 8364)",
            "utf8.codepoint('häx', 1, -1)",
            "utf8.len('häx'), utf8.len('\\xff')",
            "utf8.offset('häx', 3)",
            "select('#', collectgarbage('count')), collectgarbage('isrunning')",
            "load('return 1 + 1')()",
            "load('syntax error here')",
            "load('return x', 'chunk', 't', {x = 4})()",
            "load('return x', 'chunk', 't')",
        ]
        .map(String::from),
    );
    assert_same(&cases);
}

/// Table functions on a proxy with __index, __newindex and __len behave like
/// stock Lua, though the sandbox shifts elements in Lua.
#[test]
fn table_functions_on_proxies_match_stock_lua() {
    let proxy = "(function() local store = {1, 2, 3, 4} \
        local t = setmetatable({}, {__index = store, __newindex = store, \
        __len = function() return #store end}) OP return table.concat(store, ',') end)()";
    let cases: Vec<String> = [
        "table.insert(t, 2, 'x')",
        "table.insert(t, 5, 'x')",
        "table.insert(t, 'x')",
        "table.remove(t, 2)",
        "table.remove(t)",
        "table.remove(t, 5)",
        "table.sort(t, function(a, b) return a > b end)",
        "local _ = table.concat(t, '-')",
        "local _ = table.unpack(t)",
    ]
    .iter()
    .map(|op| proxy.replace("OP", op))
    .collect();
    assert_same(&cases);
    assert_same(&[
        "(function() local t = setmetatable({}, {__len = function() return 3 end}) \
         return pcall(function() table.insert(t, 9, 'x') end) end)()"
            .to_string(),
        "(function() local t = setmetatable({'a', 'b'}, {__len = function() return -5 end}) \
         return table.remove(t, 1), t[1], t[2] end)()"
            .to_string(),
        "(function() local t = setmetatable({'a', 'b'}, {__len = function() return 2 end}) \
         return table.remove(t, 3), table.remove(t, 2), t[1], t[2] end)()"
            .to_string(),
    ]);
}

/// Random short patterns over a small alphabet, including malformed ones.
#[test]
fn patterns_match_stock_lua_on_random_cases() {
    let tokens: &[&str] = &[
        "a", "b", ".", "%a", "%d", "[ab]", "[^a]", "*", "+", "-", "?", "(", ")", "()", "^", "$",
        "%1", "%b()", "%f[a]", "%", "[", "]", "1",
    ];
    let alphabet = b"ab(1)a ";
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    let mut cases = Vec::new();
    for _ in 0..400 {
        let pattern: String = (0..1 + next(5))
            .map(|_| tokens[next(tokens.len())])
            .collect();
        let subject: Vec<u8> = (0..next(9))
            .map(|_| alphabet[next(alphabet.len())])
            .collect();
        cases.extend(expressions_for(&subject, pattern.as_bytes()));
    }
    assert_same(&cases);
}

#[test]
fn backtracking_patterns_stop_at_the_budget() {
    let (lua, used) = sandbox(5_000_000);
    let started = Instant::now();
    let result = lua
        .load("return string.find(string.rep('a', 4000), '.-.-.-.-.-b')")
        .exec();
    assert!(result.unwrap_err().to_string().contains("budget exceeded"));
    assert!(used.get() > 5_000_000);
    assert!(started.elapsed() < Duration::from_secs(2));

    // gsub, gmatch and match are metered the same way.
    for code in [
        "string.gsub(string.rep('a', 4000), '.-.-.-.-.-b', '')",
        "for _ in string.gmatch(string.rep('a', 4000), '.-.-.-.-.-b') do end",
        "string.match(string.rep('a', 4000), '.-.-.-.-.-b')",
    ] {
        let (lua, _) = sandbox(5_000_000);
        let started = Instant::now();
        let error = lua.load(code).exec().unwrap_err().to_string();
        assert!(error.contains("budget exceeded"), "{code}: {error}");
        assert!(started.elapsed() < Duration::from_secs(2), "{code}");
    }
}

#[test]
fn plain_search_is_linear_and_charged() {
    let (lua, used) = sandbox(u64::MAX);
    let (start, end): (i64, i64) = lua
        .load("local s = string.rep('a', 1000000) .. 'b' return s:find(string.rep('a', 5000) .. 'b', 1, true)")
        .eval()
        .unwrap();
    assert_eq!((start, end), (995_001, 1_000_001));
    assert!(used.get() > 1_000_000 / 16);
}

#[test]
fn element_loops_in_library_functions_are_charged_before_they_run() {
    for code in [
        // Quadratic front insertion and removal.
        "local t = {} for i = 1, 200000 do table.insert(t, 1, i) end",
        "local t = {} for i = 1, 200000 do t[i] = i end for i = 1, 200000 do table.remove(t, 1) end",
        // One call over a huge range.
        "table.move({}, 1, 1 << 40, 2)",
        "table.concat({}, '', 1, 1 << 40)",
        // Repeated full collections of a large heap.
        "local keep = string.rep('x', 20000000) for i = 1, 1000 do collectgarbage() end",
        "local s = string.rep('x', 1000000) for i = 1, 100000 do local _ = s:upper() end",
    ] {
        let (lua, _) = sandbox(20_000_000);
        let started = Instant::now();
        let error = lua.load(code).exec().unwrap_err().to_string();
        assert!(error.contains("budget exceeded"), "{code}: {error}");
        assert!(started.elapsed() < Duration::from_secs(5), "{code}");
    }
}

#[test]
fn finalizers_bytecode_and_dump_are_unavailable() {
    let (lua, _) = sandbox(u64::MAX);
    let error = lua
        .load("setmetatable({}, {__gc = function() end})")
        .exec()
        .unwrap_err()
        .to_string();
    assert!(error.contains("__gc finalizers"), "{error}");
    // A __gc set to false at assignment could be replaced later.
    assert!(lua.load("setmetatable({}, {__gc = false})").exec().is_err());
    let (chunk, message): (LuaValue, String) = lua
        .load("return load(string.char(27) .. 'Lua')")
        .eval()
        .unwrap();
    assert!(chunk.is_nil());
    assert!(message.contains("binary chunk"), "{message}");
    let dump: LuaValue = lua.load("return string.dump").eval().unwrap();
    assert!(dump.is_nil());
}

/// Paths the review of the first version found uncharged.
#[test]
fn review_escapes_are_charged() {
    for code in [
        // Empty strings still loop once per repetition.
        "string.rep('', 1e9)",
        // A __len that grows between reads cannot buy an uncharged shift.
        "local n = 0 local t = setmetatable({}, {__len = function() n = n + 1 \
         return n == 1 and 1 or 1 << 40 end}) table.remove(t, 1) table.remove(t, 1)",
        // A long [...] class costs its length on every test.
        "string.find(string.rep('b', 20000), '[' .. string.rep('a', 100000) .. 'b]*$')",
        // A reader's pieces are charged as they are compiled.
        // (load reports a reader's error as nil plus the error.)
        "local left = 8 local f, err = load(function() left = left - 1 \
         if left > 0 then return '--' .. string.rep('x', 20000000) .. '\\n' end end) \
         if not f then error(err, 0) end",
        // Mode switches collect the whole heap.
        "local keep = string.rep('x', 20000000) for i = 1, 500 do \
         collectgarbage('generational') collectgarbage('incremental') end",
    ] {
        let (lua, _) = sandbox(20_000_000);
        let started = Instant::now();
        let Err(error) = lua.load(code).exec() else {
            panic!("ran without exceeding the budget: {code}");
        };
        assert!(
            error.to_string().contains("budget exceeded"),
            "{code}: {error}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{code}");
    }
}

#[test]
fn sorting_through_a_hidden_metatable_reads_the_length_once() {
    let (lua, used) = sandbox(u64::MAX);
    let sorted: String = lua
        .load(
            "local backing = {5, 3, 4, 1, 2}
             local reads = 0
             local t = setmetatable({}, {__metatable = {}, __index = backing,
                 __newindex = backing, __len = function() reads = reads + 1 return 5 end})
             table.sort(t)
             assert(reads == 1, reads)
             return table.concat(backing, ',')",
        )
        .eval()
        .unwrap();
    assert_eq!(sorted, "1,2,3,4,5");
    let _ = used;
}
