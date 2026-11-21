//! Check module: a block passes when its own body is not empty.
//!
//! Interface 1: the host calls `rdy_alloc(len)` for a buffer, writes the input envelope into it
//! and calls `rdy_check_v1(ptr, len)`, which returns the output's address in the high 32 bits
//! and its length in the low 32 bits.
//!
//! The input is JSON. This module reads it with a small scanner that holds no floating-point
//! code: it finds the top-level key `block`, then the key `body` of that object, and skips every
//! other value, numbers included, without reading its value. Unknown keys are ignored, and the
//! judged block's body is the only body read: a neighbour's body never stands in for it.
//! A string is scanned by its escapes, so a quote or a backslash inside text never ends it early.
//! An input that is not the expected shape fails the block.

static PASS: &[u8] = br#"{"pass":true}"#;
static FAIL: &[u8] = br#"{"pass":false}"#;

struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn white(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.white();
        if self.peek() == Some(c) {
            self.i += 1;
            Some(())
        } else {
            None
        }
    }

    /// A string at the cursor: its raw bytes between the quotes, escapes still in place.
    fn string(&mut self) -> Option<&[u8]> {
        self.white();
        if self.peek() != Some(b'"') {
            return None;
        }
        self.i += 1;
        let start = self.i;
        loop {
            match self.peek()? {
                b'"' => {
                    let raw = self.s.get(start..self.i)?;
                    self.i += 1;
                    return Some(raw);
                }
                // an escape takes the next byte: a `\"` or `\\` never ends the string
                b'\\' => self.i += 2,
                _ => self.i += 1,
            }
        }
    }

    /// Skips one value of any kind: a string, a list or mapping with all that is inside it, or a
    /// number, `true`, `false` or `null`. It counts nesting and does not recurse.
    fn skip(&mut self) -> Option<()> {
        self.white();
        let mut depth = 0usize;
        loop {
            match self.peek()? {
                b'"' => {
                    self.string()?;
                }
                b'[' | b'{' => {
                    depth += 1;
                    self.i += 1;
                }
                b']' | b'}' => {
                    // a closer with no opener is the end of the enclosing value, not ours
                    depth = depth.checked_sub(1)?;
                    self.i += 1;
                }
                // separators and bare tokens inside a list or mapping
                _ if depth > 0 => self.i += 1,
                _ => {
                    // a bare token: runs to a delimiter, and is not read
                    while !matches!(
                        self.peek(),
                        None | Some(b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
                    ) {
                        self.i += 1;
                    }
                }
            }
            if depth == 0 {
                return Some(());
            }
        }
    }
}

/// Whether the raw bytes of a key, with its escapes decoded, spell `name` (plain ASCII).
fn key_is(raw: &[u8], name: &[u8]) -> bool {
    let (mut r, mut n) = (0, 0);
    while r < raw.len() {
        let c = raw[r];
        let (v, used) = if c == b'\\' {
            match raw.get(r + 1) {
                Some(b'"') => (b'"', 2),
                Some(b'\\') => (b'\\', 2),
                Some(b'/') => (b'/', 2),
                Some(b'u') => match hex4(raw.get(r + 2..r + 6)) {
                    // only a code unit below 0x80 can spell an ASCII key
                    Some(u) if u < 0x80 => (u as u8, 6),
                    _ => return false,
                },
                // `\n`, `\t` and the like never spell a key of ours
                _ => return false,
            }
        } else {
            (c, 1)
        };
        if name.get(n) != Some(&v) {
            return false;
        }
        n += 1;
        r += used;
    }
    n == name.len()
}

fn hex4(h: Option<&[u8]>) -> Option<u32> {
    let mut v = 0u32;
    for &c in h? {
        v = v * 16 + (c as char).to_digit(16)?;
    }
    Some(v)
}

/// Whether the object at the cursor has `key` with a string value that is not empty. The other
/// members are skipped. `None` when the cursor is not at an object, or the object is cut short.
fn object_member(sc: &mut Scan, key: &[u8], on: fn(&mut Scan) -> Option<bool>) -> Option<bool> {
    sc.eat(b'{')?;
    let mut found = false;
    sc.white();
    if sc.peek() == Some(b'}') {
        return Some(false);
    }
    loop {
        let k = sc.string()?;
        let want = key_is(k, key);
        sc.eat(b':')?;
        if want {
            found = on(sc)?;
        } else {
            sc.skip()?;
        }
        sc.white();
        match sc.peek()? {
            b',' => sc.i += 1,
            b'}' => return Some(found),
            _ => return None,
        }
    }
}

fn block_body_nonempty(sc: &mut Scan) -> Option<bool> {
    object_member(sc, b"block", |sc| object_member(sc, b"body", body_text))
}

fn body_text(sc: &mut Scan) -> Option<bool> {
    Some(!sc.string()?.is_empty())
}

#[no_mangle]
pub extern "C" fn rdy_alloc(len: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as i32
}

#[no_mangle]
pub extern "C" fn rdy_check_v1(ptr: i32, len: i32) -> i64 {
    let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let mut sc = Scan { s: input, i: 0 };
    let out = if block_body_nonempty(&mut sc) == Some(true) {
        PASS
    } else {
        FAIL
    };
    ((out.as_ptr() as i64) << 32) | out.len() as i64
}
