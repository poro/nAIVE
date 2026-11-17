//! Lua 5.4 string patterns with metered work.
//!
//! Lua's own matcher runs in C, where the instruction hook cannot reach it, and
//! backtracking patterns cost polynomial time in the subject's length (a
//! 4,000-byte subject and five `.-` items never finish). This is a direct port
//! of `lstrlib.c` (`match`, `max_expand`, `min_expand`, captures, `%b`, `%f`
//! and back-references) that charges every matcher step to the caller's
//! instruction budget, so an expensive pattern stops with the CPU-limit error.
//! Plain searches use a linear-time substring search for the same reason.
//!
//! Results, errors and edge cases follow Lua 5.4; `tests.rs` compares them
//! with the C implementation.

use mlua::prelude::*;

/// Lua's limit on simultaneous captures (`LUA_MAXCAPTURES`).
const MAX_CAPTURES: usize = 32;
/// Lua's limit on matcher recursion (`MAXCCALLS`).
const MAX_MATCH_DEPTH: u32 = 200;
/// Matcher steps accumulated before charging the budget.
const FLUSH_STEPS: u64 = 1024;

/// Characters that make a `find` pattern more than a plain substring.
const SPECIALS: &[u8] = b"^$*+?.([%-";

/// A failure while matching.
pub(super) enum MatchError {
    /// Malformed pattern or argument: raised in Lua as a plain string, as
    /// the C library does.
    Pattern(String),
    /// The budget ran out (or another Lua error), propagated unchanged.
    Lua(LuaError),
}

impl From<LuaError> for MatchError {
    fn from(error: LuaError) -> Self {
        MatchError::Lua(error)
    }
}

type MatchResult<T> = Result<T, MatchError>;

fn pattern_error<T>(message: impl Into<String>) -> MatchResult<T> {
    Err(MatchError::Pattern(message.into()))
}

/// Charges matcher work to an instruction budget in batches.
pub(super) struct Meter<'a> {
    charge: &'a dyn Fn(u64) -> LuaResult<()>,
    pending: u64,
}

impl<'a> Meter<'a> {
    pub(super) fn new(charge: &'a dyn Fn(u64) -> LuaResult<()>) -> Self {
        Self { charge, pending: 0 }
    }

    #[inline]
    fn step(&mut self) -> MatchResult<()> {
        self.add(1)
    }

    #[inline]
    pub(super) fn add(&mut self, units: u64) -> MatchResult<()> {
        self.pending += units;
        if self.pending >= FLUSH_STEPS {
            self.flush()?;
        }
        Ok(())
    }

    pub(super) fn flush(&mut self) -> MatchResult<()> {
        let units = std::mem::take(&mut self.pending);
        if units > 0 {
            (self.charge)(units)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureLen {
    Unfinished,
    Position,
    Len(usize),
}

#[derive(Clone, Copy)]
struct Capture {
    init: usize,
    len: CaptureLen,
}

/// One capture result: a substring or a position (1-based).
pub(super) enum CaptureValue<'s> {
    Bytes(&'s [u8]),
    Position(usize),
}

struct MatchState<'s, 'p, 'm, 'c> {
    src: &'s [u8],
    pat: &'p [u8],
    level: usize,
    depth: u32,
    captures: [Capture; MAX_CAPTURES],
    meter: &'m mut Meter<'c>,
}

impl<'s, 'p, 'm, 'c> MatchState<'s, 'p, 'm, 'c> {
    fn new(src: &'s [u8], pat: &'p [u8], meter: &'m mut Meter<'c>) -> Self {
        Self {
            src,
            pat,
            level: 0,
            depth: MAX_MATCH_DEPTH,
            captures: [Capture {
                init: 0,
                len: CaptureLen::Unfinished,
            }; MAX_CAPTURES],
            meter,
        }
    }

    fn reset(&mut self) {
        self.level = 0;
        self.depth = MAX_MATCH_DEPTH;
    }

    /// Pattern byte at `i`, or 0 past the end (C reads the terminator).
    #[inline]
    fn p(&self, i: usize) -> u8 {
        self.pat.get(i).copied().unwrap_or(0)
    }

    /// Subject byte at `i`, or 0 past the end.
    #[inline]
    fn s(&self, i: usize) -> u8 {
        self.src.get(i).copied().unwrap_or(0)
    }

    fn class_end(&self, mut p: usize) -> MatchResult<usize> {
        let c = self.p(p);
        p += 1;
        match c {
            b'%' => {
                if p >= self.pat.len() {
                    return pattern_error("malformed pattern (ends with '%')");
                }
                Ok(p + 1)
            }
            b'[' => {
                if self.p(p) == b'^' {
                    p += 1;
                }
                loop {
                    if p >= self.pat.len() {
                        return pattern_error("malformed pattern (missing ']')");
                    }
                    let c = self.pat[p];
                    p += 1;
                    if c == b'%' && p < self.pat.len() {
                        p += 1;
                    }
                    if self.p(p) == b']' {
                        break;
                    }
                }
                Ok(p + 1)
            }
            _ => Ok(p),
        }
    }

    /// Testing a `[...]` class scans it, so long classes cost their length.
    fn single_match(&mut self, s: usize, p: usize, ep: usize) -> MatchResult<bool> {
        if s >= self.src.len() {
            return Ok(false);
        }
        let c = self.src[s];
        Ok(match self.pat[p] {
            b'.' => true,
            b'%' => match_class(c, self.p(p + 1)),
            b'[' => {
                self.meter.add(((ep - p) / 8) as u64)?;
                match_bracket_class(c, self.pat, p, ep - 1)
            }
            literal => literal == c,
        })
    }

    fn do_match(&mut self, mut s: usize, mut p: usize) -> MatchResult<Option<usize>> {
        if self.depth == 0 {
            return pattern_error("pattern too complex");
        }
        self.depth -= 1;
        let result = loop {
            self.meter.step()?;
            if p >= self.pat.len() {
                break Some(s);
            }
            match self.pat[p] {
                b'(' => {
                    break if self.p(p + 1) == b')' {
                        self.start_capture(s, p + 2, CaptureLen::Position)?
                    } else {
                        self.start_capture(s, p + 1, CaptureLen::Unfinished)?
                    };
                }
                b')' => break self.end_capture(s, p + 1)?,
                b'$' if p + 1 == self.pat.len() => {
                    break (s == self.src.len()).then_some(s);
                }
                b'%' if self.p(p + 1) == b'b' => match self.match_balance(s, p + 2)? {
                    Some(end) => {
                        s = end;
                        p += 4;
                    }
                    None => break None,
                },
                b'%' if self.p(p + 1) == b'f' => {
                    p += 2;
                    if self.p(p) != b'[' {
                        return pattern_error("missing '[' after '%f' in pattern");
                    }
                    let ep = self.class_end(p)?;
                    self.meter.add(((ep - p) / 4) as u64)?;
                    let previous = if s == 0 { 0 } else { self.src[s - 1] };
                    if !match_bracket_class(previous, self.pat, p, ep - 1)
                        && match_bracket_class(self.s(s), self.pat, p, ep - 1)
                    {
                        p = ep;
                    } else {
                        break None;
                    }
                }
                b'%' if self.p(p + 1).is_ascii_digit() => {
                    match self.match_capture(s, self.p(p + 1))? {
                        Some(end) => {
                            s = end;
                            p += 2;
                        }
                        None => break None,
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    // Finding the end of a `[...]` class scans it.
                    self.meter.add(((ep - p) / 8) as u64)?;
                    let suffix = self.p(ep);
                    if !self.single_match(s, p, ep)? {
                        if matches!(suffix, b'*' | b'?' | b'-') {
                            p = ep + 1;
                            continue;
                        }
                        break None;
                    }
                    match suffix {
                        b'?' => {
                            if let Some(end) = self.do_match(s + 1, ep + 1)? {
                                break Some(end);
                            }
                            p = ep + 1;
                        }
                        b'+' => break self.max_expand(s + 1, p, ep)?,
                        b'*' => break self.max_expand(s, p, ep)?,
                        b'-' => break self.min_expand(s, p, ep)?,
                        _ => {
                            s += 1;
                            p = ep;
                        }
                    }
                }
            }
        };
        self.depth += 1;
        Ok(result)
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> MatchResult<Option<usize>> {
        let mut i = 0;
        while self.single_match(s + i, p, ep)? {
            self.meter.step()?;
            i += 1;
        }
        loop {
            if let Some(end) = self.do_match(s + i, ep + 1)? {
                return Ok(Some(end));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> MatchResult<Option<usize>> {
        loop {
            if let Some(end) = self.do_match(s, ep + 1)? {
                return Ok(Some(end));
            }
            if self.single_match(s, p, ep)? {
                s += 1;
            } else {
                return Ok(None);
            }
        }
    }

    fn start_capture(
        &mut self,
        s: usize,
        p: usize,
        what: CaptureLen,
    ) -> MatchResult<Option<usize>> {
        let level = self.level;
        if level >= MAX_CAPTURES {
            return pattern_error("too many captures");
        }
        self.captures[level] = Capture { init: s, len: what };
        self.level = level + 1;
        let result = self.do_match(s, p)?;
        if result.is_none() {
            self.level -= 1;
        }
        Ok(result)
    }

    fn end_capture(&mut self, s: usize, p: usize) -> MatchResult<Option<usize>> {
        let Some(l) = (0..self.level)
            .rev()
            .find(|&l| self.captures[l].len == CaptureLen::Unfinished)
        else {
            return pattern_error("invalid pattern capture");
        };
        self.captures[l].len = CaptureLen::Len(s - self.captures[l].init);
        let result = self.do_match(s, p)?;
        if result.is_none() {
            self.captures[l].len = CaptureLen::Unfinished;
        }
        Ok(result)
    }

    fn check_capture(&self, digit: u8) -> MatchResult<usize> {
        let l = i32::from(digit) - i32::from(b'1');
        if l < 0
            || l as usize >= self.level
            || self.captures[l as usize].len == CaptureLen::Unfinished
        {
            return pattern_error(format!("invalid capture index %{}", l + 1));
        }
        Ok(l as usize)
    }

    fn match_capture(&mut self, s: usize, digit: u8) -> MatchResult<Option<usize>> {
        let l = self.check_capture(digit)?;
        let CaptureLen::Len(len) = self.captures[l].len else {
            // A position capture never matches as a back-reference.
            return Ok(None);
        };
        self.meter.add(len as u64 / 16)?;
        let init = self.captures[l].init;
        if self.src.len() - s >= len && self.src[init..init + len] == self.src[s..s + len] {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    fn match_balance(&mut self, s: usize, p: usize) -> MatchResult<Option<usize>> {
        if p + 1 >= self.pat.len() {
            return pattern_error("malformed pattern (missing arguments to '%b')");
        }
        if self.s(s) != self.pat[p] {
            return Ok(None);
        }
        let (open, close) = (self.pat[p], self.pat[p + 1]);
        let mut depth = 1;
        let mut s = s + 1;
        while s < self.src.len() {
            self.meter.step()?;
            let c = self.src[s];
            if c == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(s + 1));
                }
            } else if c == open {
                depth += 1;
            }
            s += 1;
        }
        Ok(None)
    }

    fn capture(&self, i: usize, s: usize, e: usize) -> MatchResult<CaptureValue<'s>> {
        if i >= self.level {
            if i != 0 {
                return pattern_error(format!("invalid capture index %{}", i + 1));
            }
            return Ok(CaptureValue::Bytes(&self.src[s..e]));
        }
        let capture = self.captures[i];
        match capture.len {
            CaptureLen::Unfinished => pattern_error("unfinished capture"),
            CaptureLen::Position => Ok(CaptureValue::Position(capture.init + 1)),
            CaptureLen::Len(len) => Ok(CaptureValue::Bytes(
                &self.src[capture.init..capture.init + len],
            )),
        }
    }

    /// Captures as `push_captures` returns them. With `whole` set, a pattern
    /// without captures yields the whole match.
    fn captures(&self, s: usize, e: usize, whole: bool) -> MatchResult<Vec<CaptureValue<'s>>> {
        let count = if self.level == 0 && whole {
            1
        } else {
            self.level
        };
        (0..count).map(|i| self.capture(i, s, e)).collect()
    }
}

fn match_class(c: u8, class: u8) -> bool {
    let result = match class.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        // C isspace includes vertical tab, which is_ascii_whitespace does not.
        b's' => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        // Deprecated but still accepted by Lua 5.4: the NUL byte.
        b'z' => c == 0,
        _ => return class == c,
    };
    if class.is_ascii_uppercase() {
        !result
    } else {
        result
    }
}

/// `p` is the class's `[`, `end` its closing `]`.
fn match_bracket_class(c: u8, pat: &[u8], mut p: usize, end: usize) -> bool {
    let at = |i: usize| pat.get(i).copied().unwrap_or(0);
    let mut found = true;
    if at(p + 1) == b'^' {
        found = false;
        p += 1;
    }
    loop {
        p += 1;
        if p >= end {
            break;
        }
        if at(p) == b'%' {
            p += 1;
            if match_class(c, at(p)) {
                return found;
            }
        } else if at(p + 1) == b'-' && p + 2 < end {
            p += 2;
            if at(p - 2) <= c && c <= at(p) {
                return found;
            }
        } else if at(p) == c {
            return found;
        }
    }
    !found
}

/// `posrelatI`: a 1-based start position, clamped like Lua's.
pub(super) fn start_position(pos: i64, len: usize) -> usize {
    if pos > 0 {
        pos as usize
    } else if pos == 0 || pos < -(len as i64) {
        1
    } else {
        (len as i64 + pos + 1) as usize
    }
}

/// What a successful `find`/`match` returns.
pub(super) enum Found<'s> {
    /// `find`: 1-based start and end, then explicit captures.
    Span(usize, usize, Vec<CaptureValue<'s>>),
    /// `match`: the captures (or the whole match).
    Captures(Vec<CaptureValue<'s>>),
}

/// `string.find` (with `find`) or `string.match` from a 0-based `init`.
pub(super) fn find<'s>(
    meter: &mut Meter<'_>,
    src: &'s [u8],
    pat: &[u8],
    init: usize,
    plain: bool,
    find: bool,
) -> MatchResult<Option<Found<'s>>> {
    if find && (plain || !pat.iter().any(|b| SPECIALS.contains(b))) {
        meter.add(((src.len() - init) + pat.len()) as u64 / 16 + 1)?;
        meter.flush()?;
        return Ok(memchr::memmem::find(&src[init..], pat).map(|offset| {
            let start = init + offset;
            Found::Span(start + 1, start + pat.len(), Vec::new())
        }));
    }
    let (anchor, pat) = match pat.first() {
        Some(b'^') => (true, &pat[1..]),
        _ => (false, pat),
    };
    let mut state = MatchState::new(src, pat, meter);
    let mut start = init;
    loop {
        state.reset();
        if let Some(end) = state.do_match(start, 0)? {
            let found = if find {
                Found::Span(start + 1, end, state.captures(start, end, false)?)
            } else {
                Found::Captures(state.captures(start, end, true)?)
            };
            state.meter.flush()?;
            return Ok(Some(found));
        }
        if start >= src.len() || anchor {
            break;
        }
        start += 1;
    }
    state.meter.flush()?;
    Ok(None)
}

/// The next match at or after 0-based `from` whose end differs from
/// `last_end`, as `gmatch` and `gsub` iterate. Returns 0-based start and end
/// with the captures (or the whole match).
pub(super) fn next_match<'s>(
    meter: &mut Meter<'_>,
    src: &'s [u8],
    pat: &[u8],
    from: usize,
    last_end: Option<usize>,
    anchor: bool,
) -> MatchResult<Option<(usize, usize, Vec<CaptureValue<'s>>)>> {
    let mut state = MatchState::new(src, pat, meter);
    let mut start = from;
    while start <= src.len() {
        state.reset();
        if let Some(end) = state.do_match(start, 0)? {
            if Some(end) != last_end {
                let captures = state.captures(start, end, true)?;
                state.meter.flush()?;
                return Ok(Some((start, end, captures)));
            }
        }
        if anchor {
            break;
        }
        start += 1;
    }
    state.meter.flush()?;
    Ok(None)
}

/// `gsub` with a string replacement, entirely in Rust. Returns the result
/// (None when nothing was replaced) and the number of matches.
pub(super) fn gsub_string(
    meter: &mut Meter<'_>,
    src: &[u8],
    pat: &[u8],
    replacement: &[u8],
    max_replacements: i64,
) -> MatchResult<(Option<Vec<u8>>, i64)> {
    let (anchor, pat) = match pat.first() {
        Some(b'^') => (true, &pat[1..]),
        _ => (false, pat),
    };
    let mut out = Vec::new();
    let mut count = 0i64;
    let mut position = 0;
    let mut last_end = None;
    let mut state = MatchState::new(src, pat, meter);
    while count < max_replacements {
        state.reset();
        let matched = state
            .do_match(position, 0)?
            .filter(|&end| Some(end) != last_end);
        if let Some(end) = matched {
            count += 1;
            append_replacement(&mut state, &mut out, replacement, position, end)?;
            position = end;
            last_end = Some(end);
        } else if position < src.len() {
            out.push(src[position]);
            position += 1;
        } else {
            break;
        }
        if anchor {
            break;
        }
    }
    state.meter.flush()?;
    if count == 0 {
        return Ok((None, 0));
    }
    out.extend_from_slice(&src[position..]);
    Ok((Some(out), count))
}

fn append_replacement(
    state: &mut MatchState<'_, '_, '_, '_>,
    out: &mut Vec<u8>,
    replacement: &[u8],
    s: usize,
    e: usize,
) -> MatchResult<()> {
    state.meter.add(replacement.len() as u64 / 16)?;
    let mut i = 0;
    while i < replacement.len() {
        let c = replacement[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let escaped = replacement.get(i).copied().unwrap_or(0);
        i += 1;
        match escaped {
            b'%' => out.push(b'%'),
            b'0' => out.extend_from_slice(&state.src[s..e]),
            b'1'..=b'9' => match state.capture(usize::from(escaped - b'1'), s, e)? {
                CaptureValue::Bytes(bytes) => out.extend_from_slice(bytes),
                CaptureValue::Position(position) => {
                    out.extend_from_slice(position.to_string().as_bytes())
                }
            },
            _ => return pattern_error("invalid use of '%' in replacement string"),
        }
    }
    Ok(())
}
