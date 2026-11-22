//! The output of a check module, interface 1: one JSON object, `{"pass":true}`, or
//! `{"pass":false}` with an optional `message` and `fix`, each a string of 1 to
//! [`TEXT_CHARS_MAX`] characters. Anything else is not a check result.
//!
//! The reader is strict and reads the bytes once, from the front. JSON whitespace may come
//! between tokens. A key may come once, and only `pass`, `message` and `fix` are keys. Nothing
//! may follow the object. The first problem met is the detail, in fixed words that never quote
//! the module's bytes. The bytes come from guest memory and are at most
//! [`crate::config::OUTPUT_BYTES_MAX`] long, so no string the reader makes is longer.
use crate::CheckResult;

/// The most Unicode scalar values a result's `message` or `fix` may hold.
pub const TEXT_CHARS_MAX: usize = 2000;

/// The check result that the output `bytes` hold. Err: why they hold none, as the detail of
/// `the output is not a check result: <detail>`.
pub fn decode_result(bytes: &[u8]) -> Result<CheckResult, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "it is not UTF-8".to_string())?;
    Reader { text, at: 0 }.result().map_err(str::to_string)
}

/// A detail in fixed words.
type Detail = &'static str;

const NOT_JSON: Detail = "it is not JSON";
const NOT_OBJECT: Detail = "it is not a JSON object";

/// A key of the result object.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Pass,
    Message,
    Fix,
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn result(&mut self) -> Result<CheckResult, Detail> {
        self.space();
        if !self.eat(b'{') {
            return Err(NOT_OBJECT);
        }
        let mut pass: Option<bool> = None;
        let mut message: Option<String> = None;
        let mut fix: Option<String> = None;
        let mut seen: Vec<Key> = Vec::with_capacity(3);
        self.space();
        if !self.eat(b'}') {
            loop {
                self.space();
                let key = match self.string()?.as_str() {
                    "pass" => Key::Pass,
                    "message" => Key::Message,
                    "fix" => Key::Fix,
                    _ => return Err("it has a key other than pass, message and fix"),
                };
                if seen.contains(&key) {
                    return Err("it gives a key twice");
                }
                seen.push(key);
                self.space();
                if !self.eat(b':') {
                    return Err(NOT_JSON);
                }
                self.space();
                match key {
                    Key::Pass => pass = Some(self.boolean()?),
                    Key::Message => message = Some(self.text_value("message")?),
                    Key::Fix => fix = Some(self.text_value("fix")?),
                }
                self.space();
                if self.eat(b',') {
                    continue;
                }
                if self.eat(b'}') {
                    break;
                }
                return Err(NOT_JSON);
            }
        }
        self.space();
        if self.at != self.text.len() {
            return Err("it has more after the JSON object");
        }
        match pass {
            None => Err("it lacks pass"),
            Some(true) if message.is_some() || fix.is_some() => {
                Err("a passing result has a message or fix")
            }
            Some(true) => Ok(CheckResult::Pass),
            Some(false) => Ok(CheckResult::Fail { message, fix }),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.at += 1;
            return true;
        }
        false
    }

    /// Skips JSON whitespace: space, tab, line feed and carriage return.
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// `true` or `false`.
    fn boolean(&mut self) -> Result<bool, Detail> {
        let rest = &self.text[self.at..];
        for (word, value) in [("true", true), ("false", false)] {
            if rest.starts_with(word) {
                self.at += word.len();
                return Ok(value);
            }
        }
        Err("pass is not true or false")
    }

    /// The string value of `key`, of 1 to [`TEXT_CHARS_MAX`] characters.
    fn text_value(&mut self, key: &'static str) -> Result<String, Detail> {
        if self.peek() != Some(b'"') {
            return Err(match key {
                "message" => "message is not a string",
                _ => "fix is not a string",
            });
        }
        let s = self.string()?;
        let n = s.chars().take(TEXT_CHARS_MAX + 1).count();
        if n == 0 || n > TEXT_CHARS_MAX {
            return Err(match key {
                "message" => "message is not 1 to 2000 characters",
                _ => "fix is not 1 to 2000 characters",
            });
        }
        Ok(s)
    }

    /// A JSON string, its escapes decoded. A control character must be escaped, and a `\u`
    /// escape of a surrogate must pair a high one with a low one.
    fn string(&mut self) -> Result<String, Detail> {
        if !self.eat(b'"') {
            return Err(NOT_JSON);
        }
        let mut out = String::new();
        loop {
            let rest = &self.text[self.at..];
            let Some(c) = rest.chars().next() else {
                return Err(NOT_JSON);
            };
            self.at += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => out.push(self.escape()?),
                c if u32::from(c) < 0x20 => return Err(NOT_JSON),
                c => out.push(c),
            }
        }
    }

    /// The character an escape after its backslash stands for.
    fn escape(&mut self) -> Result<char, Detail> {
        let Some(b) = self.peek() else {
            return Err(NOT_JSON);
        };
        self.at += 1;
        Ok(match b {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let high = self.hex4()?;
                let code = match high {
                    0xd800..=0xdbff => {
                        if !(self.eat(b'\\') && self.eat(b'u')) {
                            return Err(NOT_JSON);
                        }
                        let low = self.hex4()?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err(NOT_JSON);
                        }
                        0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                    }
                    0xdc00..=0xdfff => return Err(NOT_JSON),
                    code => code,
                };
                char::from_u32(code).ok_or(NOT_JSON)?
            }
            _ => return Err(NOT_JSON),
        })
    }

    /// Four hex digits, of either case.
    fn hex4(&mut self) -> Result<u32, Detail> {
        let digits = self.text.get(self.at..self.at + 4).ok_or(NOT_JSON)?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(NOT_JSON);
        }
        self.at += 4;
        u32::from_str_radix(digits, 16).map_err(|_| NOT_JSON)
    }
}
