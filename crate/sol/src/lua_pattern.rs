//! Lua's native pattern-matching engine.
//!
//! This is not a regex engine: it implements Lua's own pattern syntax
//! (`%a`/`%d`/`%s`-style character classes, `[...]` sets, `^`/`$` anchors,
//! `()`-captures including position captures, `%1`-`%9` back-references,
//! `*`/`+`/`-`/`?` quantifiers, and `%b`/`%f` balanced/frontier patterns) as
//! specified in the Lua manual, operating on raw bytes to stay consistent
//! with `lua_runtime.rs`'s byte-string `LuaValue::String`.

const MAX_CAPTURES: usize = 32;
const CAP_UNFINISHED: isize = -1;
const CAP_POSITION: isize = -2;
const MAX_MATCH_DEPTH: i32 = 200;

type MResult<T> = Result<T, String>;

#[derive(Clone, Copy)]
struct CaptureInfo {
    start: usize,
    len: isize,
}

/// One capture result: either a byte range of the source or a `()` position
/// capture (already 1-based, per Lua convention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    Str(usize, usize),
    Position(usize),
}

/// A successful match: `start`/`end` are 0-based byte offsets into the
/// source, with `end` exclusive (i.e. `src[start..end]` is the whole match).
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub captures: Vec<Capture>,
}

struct MatchState<'a> {
    src: &'a [u8],
    pat: &'a [u8],
    level: usize,
    capture: [CaptureInfo; MAX_CAPTURES],
    depth: i32,
}

impl<'a> MatchState<'a> {
    fn new(src: &'a [u8], pat: &'a [u8]) -> Self {
        Self {
            src,
            pat,
            level: 0,
            capture: [CaptureInfo { start: 0, len: 0 }; MAX_CAPTURES],
            depth: MAX_MATCH_DEPTH,
        }
    }

    fn is_space(c: u8) -> bool {
        matches!(c, b' ' | 0x09..=0x0D)
    }

    fn match_class(c: u8, cl: u8) -> bool {
        let result = match cl.to_ascii_lowercase() {
            b'a' => c.is_ascii_alphabetic(),
            b'd' => c.is_ascii_digit(),
            b'l' => c.is_ascii_lowercase(),
            b's' => Self::is_space(c),
            b'u' => c.is_ascii_uppercase(),
            b'w' => c.is_ascii_alphanumeric(),
            b'c' => c.is_ascii_control(),
            b'p' => c.is_ascii_graphic() && !c.is_ascii_alphanumeric(),
            b'x' => c.is_ascii_hexdigit(),
            b'g' => c.is_ascii_graphic(),
            b'z' => c == 0,
            _ => return c == cl,
        };
        if cl.is_ascii_uppercase() {
            !result
        } else {
            result
        }
    }

    /// Given `p` pointing at the start of a single pattern item (a literal
    /// byte, `%x` escape, or `[...]` set), return the index right after it.
    fn class_end(&self, mut p: usize) -> MResult<usize> {
        let c = self.pat[p];
        p += 1;
        match c {
            b'%' => {
                if p >= self.pat.len() {
                    return Err("malformed pattern (ends with '%')".into());
                }
                Ok(p + 1)
            }
            b'[' => {
                if p < self.pat.len() && self.pat[p] == b'^' {
                    p += 1;
                }
                loop {
                    if p >= self.pat.len() {
                        return Err("malformed pattern (missing ']')".into());
                    }
                    let ch = self.pat[p];
                    p += 1;
                    if ch == b'%' {
                        if p >= self.pat.len() {
                            return Err("malformed pattern (ends with '%')".into());
                        }
                        p += 1;
                    }
                    if p < self.pat.len() && self.pat[p] == b']' {
                        break;
                    }
                    if p >= self.pat.len() {
                        return Err("malformed pattern (missing ']')".into());
                    }
                }
                Ok(p + 1)
            }
            _ => Ok(p),
        }
    }

    fn match_bracket_class(&self, c: u8, p: usize, ec: usize) -> bool {
        let mut p = p + 1;
        let mut negate = false;
        if p < ec && self.pat[p] == b'^' {
            negate = true;
            p += 1;
        }
        while p < ec {
            if self.pat[p] == b'%' {
                p += 1;
                if p < ec && Self::match_class(c, self.pat[p]) {
                    return !negate;
                }
                p += 1;
            } else if p + 2 < ec && self.pat[p + 1] == b'-' {
                if self.pat[p] <= c && c <= self.pat[p + 2] {
                    return !negate;
                }
                p += 3;
            } else {
                if self.pat[p] == c {
                    return !negate;
                }
                p += 1;
            }
        }
        negate
    }

    /// Whether `src[s]` (if in bounds) matches the pattern item `pat[p..ep)`.
    fn single_match(&self, s: usize, p: usize, ep: usize) -> bool {
        if s >= self.src.len() {
            return false;
        }
        let c = self.src[s];
        match self.pat[p] {
            b'.' => true,
            b'%' => Self::match_class(c, self.pat[p + 1]),
            b'[' => self.match_bracket_class(c, p, ep - 1),
            literal => literal == c,
        }
    }

    fn match_balance(&self, s: usize, p: usize) -> MResult<Option<usize>> {
        if p + 1 >= self.pat.len() {
            return Err("malformed pattern (missing arguments to '%b')".into());
        }
        if s >= self.src.len() || self.src[s] != self.pat[p] {
            return Ok(None);
        }
        let (open, close) = (self.pat[p], self.pat[p + 1]);
        let mut depth = 1;
        let mut i = s + 1;
        while i < self.src.len() {
            if self.src[i] == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(i + 1));
                }
            } else if self.src[i] == open {
                depth += 1;
            }
            i += 1;
        }
        Ok(None)
    }

    fn check_capture(&self, digit: usize) -> MResult<usize> {
        if digit == 0 || digit > self.level {
            return Err(format!("invalid capture index %{digit}"));
        }
        let index = digit - 1;
        if self.capture[index].len == CAP_UNFINISHED {
            return Err(format!("invalid capture index %{digit}"));
        }
        Ok(index)
    }

    fn capture_to_close(&self) -> MResult<usize> {
        let mut level = self.level;
        while level > 0 {
            level -= 1;
            if self.capture[level].len == CAP_UNFINISHED {
                return Ok(level);
            }
        }
        Err("invalid pattern capture".into())
    }

    fn match_capture(&self, s: usize, digit: usize) -> MResult<Option<usize>> {
        let index = self.check_capture(digit)?;
        let start = self.capture[index].start;
        let len = self.capture[index].len as usize;
        if self.src.len().saturating_sub(s) >= len
            && self.src[start..start + len] == self.src[s..s + len]
        {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    fn start_capture(&mut self, s: usize, p: usize, what: isize) -> MResult<Option<usize>> {
        let level = self.level;
        if level >= MAX_CAPTURES {
            return Err("too many captures".into());
        }
        self.capture[level] = CaptureInfo {
            start: s,
            len: what,
        };
        self.level += 1;
        let result = self.do_match(s, p)?;
        if result.is_none() {
            self.level -= 1;
        }
        Ok(result)
    }

    fn end_capture(&mut self, s: usize, p: usize) -> MResult<Option<usize>> {
        let index = self.capture_to_close()?;
        self.capture[index].len = (s - self.capture[index].start) as isize;
        let result = self.do_match(s, p)?;
        if result.is_none() {
            self.capture[index].len = CAP_UNFINISHED;
        }
        Ok(result)
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> MResult<Option<usize>> {
        let mut count = 0usize;
        while self.single_match(s + count, p, ep) {
            count += 1;
        }
        loop {
            if let Some(result) = self.do_match(s + count, ep + 1)? {
                return Ok(Some(result));
            }
            if count == 0 {
                return Ok(None);
            }
            count -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> MResult<Option<usize>> {
        loop {
            if let Some(result) = self.do_match(s, ep + 1)? {
                return Ok(Some(result));
            } else if self.single_match(s, p, ep) {
                s += 1;
            } else {
                return Ok(None);
            }
        }
    }

    fn do_match(&mut self, s: usize, p: usize) -> MResult<Option<usize>> {
        self.depth -= 1;
        if self.depth < 0 {
            self.depth += 1;
            return Err("pattern too complex".into());
        }
        let result = self.match_step(s, p);
        self.depth += 1;
        result
    }

    fn match_step(&mut self, mut s: usize, mut p: usize) -> MResult<Option<usize>> {
        loop {
            if p >= self.pat.len() {
                return Ok(Some(s));
            }
            match self.pat[p] {
                b'(' => {
                    return if self.pat.get(p + 1) == Some(&b')') {
                        self.start_capture(s, p + 2, CAP_POSITION)
                    } else {
                        self.start_capture(s, p + 1, CAP_UNFINISHED)
                    };
                }
                b')' => return self.end_capture(s, p + 1),
                b'$' if p + 1 == self.pat.len() => {
                    return Ok((s == self.src.len()).then_some(s));
                }
                b'%' if self.pat.get(p + 1) == Some(&b'b') => {
                    match self.match_balance(s, p + 2)? {
                        Some(next) => {
                            s = next;
                            p += 4;
                            continue;
                        }
                        None => return Ok(None),
                    }
                }
                b'%' if self.pat.get(p + 1) == Some(&b'f') => {
                    let fp = p + 2;
                    if self.pat.get(fp) != Some(&b'[') {
                        return Err("missing '[' after '%f' in pattern".into());
                    }
                    let ep = self.class_end(fp)?;
                    let previous = if s == 0 { 0u8 } else { self.src[s - 1] };
                    let current = if s < self.src.len() { self.src[s] } else { 0u8 };
                    if !self.match_bracket_class(previous, fp, ep - 1)
                        && self.match_bracket_class(current, fp, ep - 1)
                    {
                        p = ep;
                        continue;
                    }
                    return Ok(None);
                }
                b'%' if matches!(self.pat.get(p + 1), Some(c) if c.is_ascii_digit()) => {
                    let digit = (self.pat[p + 1] - b'0') as usize;
                    match self.match_capture(s, digit)? {
                        Some(next) => {
                            s = next;
                            p += 2;
                            continue;
                        }
                        None => return Ok(None),
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    let matched = self.single_match(s, p, ep);
                    match self.pat.get(ep) {
                        Some(b'?') => {
                            if matched {
                                if let Some(result) = self.do_match(s + 1, ep + 1)? {
                                    return Ok(Some(result));
                                }
                            }
                            p = ep + 1;
                            continue;
                        }
                        Some(b'+') => {
                            return if matched {
                                self.max_expand(s + 1, p, ep)
                            } else {
                                Ok(None)
                            };
                        }
                        Some(b'*') => return self.max_expand(s, p, ep),
                        Some(b'-') => return self.min_expand(s, p, ep),
                        _ => {
                            if !matched {
                                return Ok(None);
                            }
                            s += 1;
                            p = ep;
                            continue;
                        }
                    }
                }
            }
        }
    }

    fn collect_captures(&self) -> MResult<Vec<Capture>> {
        let mut out = Vec::with_capacity(self.level);
        for index in 0..self.level {
            let capture = self.capture[index];
            if capture.len == CAP_POSITION {
                out.push(Capture::Position(capture.start + 1));
            } else if capture.len == CAP_UNFINISHED {
                return Err("unfinished capture".into());
            } else {
                out.push(Capture::Str(
                    capture.start,
                    capture.start + capture.len as usize,
                ));
            }
        }
        Ok(out)
    }
}

/// Try to match `pat` starting at exactly byte offset `s` (no scanning, and
/// `pat` is used literally — a leading `^` is not treated as an anchor here).
/// This is the primitive `string.gmatch` iterates with, since gmatch's
/// pattern keeps a leading `^` as a literal character per the Lua manual.
pub fn match_at(src: &[u8], pat: &[u8], s: usize) -> MResult<Option<Match>> {
    if s > src.len() {
        return Ok(None);
    }
    let mut state = MatchState::new(src, pat);
    match state.do_match(s, 0)? {
        Some(end) => {
            let captures = state.collect_captures()?;
            Ok(Some(Match {
                start: s,
                end,
                captures,
            }))
        }
        None => Ok(None),
    }
}

/// Search `src` for `pat` starting no earlier than byte offset `init`
/// (already clamped into `0..=src.len()` by the caller). A pattern starting
/// with `^` is anchored to `init` only (mirroring `string.find`/`match`).
pub fn find(src: &[u8], pat: &[u8], init: usize) -> MResult<Option<Match>> {
    let (anchored, body) = match pat.first() {
        Some(b'^') => (true, &pat[1..]),
        _ => (false, pat),
    };
    let mut s = init.min(src.len());
    loop {
        if let Some(m) = match_at(src, body, s)? {
            return Ok(Some(m));
        }
        if anchored || s >= src.len() {
            return Ok(None);
        }
        s += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_str(src: &str, pat: &str) -> Option<(usize, usize)> {
        find(src.as_bytes(), pat.as_bytes(), 0)
            .unwrap()
            .map(|m| (m.start, m.end))
    }

    #[test]
    fn plain_literal() {
        assert_eq!(find_str("hello world", "world"), Some((6, 11)));
        assert_eq!(find_str("hello world", "xyz"), None);
    }

    #[test]
    fn classes_and_quantifiers() {
        assert_eq!(find_str("   123abc", "%d+"), Some((3, 6)));
        assert_eq!(find_str("   123abc", "%a+"), Some((6, 9)));
        assert_eq!(find_str("abc", "a.-c"), Some((0, 3)));
        assert_eq!(find_str("", "%d*"), Some((0, 0)));
        assert_eq!(
            find(b"\0x", b"%z", 0).unwrap().map(|m| (m.start, m.end)),
            Some((0, 1))
        );
        assert_eq!(
            find(b"\0x", b"%Z", 0).unwrap().map(|m| (m.start, m.end)),
            Some((1, 2))
        );
    }

    #[test]
    fn anchors() {
        assert_eq!(find_str("hello", "^hel"), Some((0, 3)));
        assert_eq!(find_str("xhello", "^hel"), None);
        assert_eq!(find_str("hello", "llo$"), Some((2, 5)));
        assert_eq!(find_str("helloz", "llo$"), None);
    }

    #[test]
    fn sets() {
        assert_eq!(find_str("cat", "[abc]at"), Some((0, 3)));
        assert_eq!(find_str("hat", "[^abc]at"), Some((0, 3)));
        assert_eq!(find_str("a5b", "[0-9]"), Some((1, 2)));
    }

    #[test]
    fn captures() {
        let m = find(b"key=value", b"(%a+)=(%a+)", 0).unwrap().unwrap();
        assert_eq!(m.captures.len(), 2);
        assert_eq!(m.captures[0], Capture::Str(0, 3));
        assert_eq!(m.captures[1], Capture::Str(4, 9));
    }

    #[test]
    fn position_capture() {
        let m = find(b"abc", b"a()b", 0).unwrap().unwrap();
        assert_eq!(m.captures, vec![Capture::Position(2)]);
    }

    #[test]
    fn balanced_match() {
        assert_eq!(find_str("(foo(bar))baz", "%b()"), Some((0, 10)));
    }

    #[test]
    fn backreference() {
        assert_eq!(find_str("abcabc", "(abc)%1"), Some((0, 6)));
        assert_eq!(find_str("abcabd", "(abc)%1"), None);
    }

    #[test]
    fn frontier() {
        assert_eq!(find_str("THE (quick) fox", "%f[%a]%u+%f[%A]"), Some((0, 3)));
    }
}
