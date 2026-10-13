//! Sibling order keys: base-62 strings compared byte-wise. Appends use a counter form that grows
//! logarithmically; inserts between siblings use midpoints. Keys are capped at 24 characters.
const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
pub const MAX_ORDER_KEY_LEN: usize = 24;
/// Counter classes: 'a' (1 digit) .. 'm' (13 digits).
const FIRST_CLASS: usize = 36;
const LAST_CLASS: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrderError {
    #[error("order key {0:?} is malformed: use 1-24 characters from 0-9A-Za-z, not ending in '0'")]
    Malformed(String),
    #[error("no order key fits between {0:?} and {1:?}")]
    NoRoom(String, String),
    #[error("an order key here would need more than {MAX_ORDER_KEY_LEN} characters")]
    TooDeep,
}

fn idx(c: u8) -> Option<usize> {
    DIGITS.iter().position(|&d| d == c)
}

/// A digit's value; only called on validated keys.
fn digit(c: u8) -> usize {
    idx(c).expect("order key digit outside base-62")
}

pub fn valid_order_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_ORDER_KEY_LEN
        && k.bytes().all(|b| idx(b).is_some())
        && !k.ends_with('0')
}

fn class_digits(class: usize) -> usize {
    class - FIRST_CLASS + 1
}

fn first_of_class(class: usize) -> String {
    let n = class_digits(class);
    let mut s = String::with_capacity(n + 1);
    s.push(DIGITS[class] as char);
    s.extend(std::iter::repeat_n('0', n - 1));
    s.push('1');
    s
}

/// The smallest n-digit base-62 string greater than `t` that doesn't end in '0', if any.
fn digits_after(t: &[u8], n: usize) -> Option<Vec<u8>> {
    let mut d: Vec<usize> = t.iter().take(n).map(|&c| idx(c).unwrap_or(0)).collect();
    if t.len() < n {
        // `t` is a proper prefix of its padding: pad with zeros, then end in '1'
        d.resize(n, 0);
        d[n - 1] = 1;
        return Some(d.into_iter().map(|i| DIGITS[i]).collect());
    }
    // t.len() >= n: need d > t[..n] (equal means d is a prefix of t, i.e. not greater)
    loop {
        let mut i = n;
        loop {
            if i == 0 {
                return None;
            }
            i -= 1;
            if d[i] < 61 {
                d[i] += 1;
                d.truncate(i + 1);
                d.resize(n, 0);
                break;
            }
        }
        if d[n - 1] != 0 {
            return Some(d.into_iter().map(|i| DIGITS[i]).collect());
        }
    }
}

/// A key greater than `a` (a valid key, or none).
fn append(a: Option<&str>) -> String {
    let Some(a) = a else {
        return first_of_class(FIRST_CLASS);
    };
    if a < "a1" {
        return first_of_class(FIRST_CLASS);
    }
    let bytes = a.as_bytes();
    let c = digit(bytes[0]);
    if (FIRST_CLASS..=LAST_CLASS).contains(&c) {
        if let Some(d) = digits_after(&bytes[1..], class_digits(c)) {
            let mut s = vec![bytes[0]];
            s.extend(d);
            return String::from_utf8(s).expect("ascii");
        }
        if c < LAST_CLASS {
            return first_of_class(c + 1);
        }
    }
    // M1-era keys above the counter classes (and a full last class): the old midpoint append
    String::from_utf8(midpoint(bytes, None)).expect("ascii")
}

/// A key strictly between `a` and `b` (either may be open). Appends (`b = None`) use the
/// counter form; everything else takes a midpoint.
pub fn order_between(a: Option<&str>, b: Option<&str>) -> Result<String, OrderError> {
    for k in a.iter().chain(b.iter()) {
        if !valid_order_key(k) {
            return Err(OrderError::Malformed(k.to_string()));
        }
    }
    let key = match (a, b) {
        (_, None) => append(a),
        (Some(a), Some(b)) if a >= b => return Err(OrderError::NoRoom(a.into(), b.into())),
        (a, Some(b)) => String::from_utf8(midpoint(a.unwrap_or("").as_bytes(), Some(b.as_bytes())))
            .expect("ascii"),
    };
    if key.len() > MAX_ORDER_KEY_LEN {
        Err(OrderError::TooDeep)
    } else {
        Ok(key)
    }
}

/// `n` short keys, increasing, all strictly between `lo` and `hi` (either may be open). The
/// engine uses it to re-key a run of siblings that share one key, so a block can go between
/// two of them. Bounds are validated; equal or reversed bounds are `NoRoom`, a result too long
/// for the key cap is `TooDeep`; it never panics.
pub fn order_run(lo: Option<&str>, hi: Option<&str>, n: usize) -> Result<Vec<String>, OrderError> {
    for k in lo.iter().chain(hi.iter()) {
        if !valid_order_key(k) {
            return Err(OrderError::Malformed(k.to_string()));
        }
    }
    if let (Some(a), Some(b)) = (lo, hi)
        && a >= b
    {
        return Err(OrderError::NoRoom(a.into(), b.into()));
    }
    let mut out = Vec::with_capacity(n);
    match hi {
        Some(h) => spread(lo, h, n, &mut out)?,
        None => {
            let mut prev = lo.map(str::to_string);
            for _ in 0..n {
                let k = order_between(prev.as_deref(), None)?;
                prev = Some(k.clone());
                out.push(k);
            }
        }
    }
    Ok(out)
}

/// New keys for a parent's children in order: `Some(k)` items are fixed anchors and keep `k`;
/// `None` items get short keys between their neighbouring anchors.
pub fn rebalance_keys(items: &[Option<String>]) -> Result<Vec<String>, OrderError> {
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        if let Some(k) = &items[i] {
            out.push(k.clone());
            i += 1;
            continue;
        }
        let start = i;
        while i < items.len() && items[i].is_none() {
            i += 1;
        }
        let lo = out.last().cloned();
        match items.get(i).cloned().flatten() {
            None => {
                let mut prev = lo;
                for _ in start..i {
                    let k = order_between(prev.as_deref(), None)?;
                    prev = Some(k.clone());
                    out.push(k);
                }
            }
            Some(h) => spread(lo.as_deref(), &h, i - start, &mut out)?,
        }
    }
    Ok(out)
}

/// `k` keys strictly between `lo` and `hi`, by balanced bisection.
fn spread(lo: Option<&str>, hi: &str, k: usize, out: &mut Vec<String>) -> Result<(), OrderError> {
    if k == 0 {
        return Ok(());
    }
    let mid = order_between(lo, Some(hi))?;
    let left = (k - 1) / 2;
    spread(lo, &mid, left, out)?;
    out.push(mid.clone());
    spread(Some(&mid), hi, k - 1 - left, out)
}

fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            assert!(n < b.len(), "order keys must not end in '0'");
            let rest_a = if n < a.len() { &a[n..] } else { &[][..] };
            let mut out = b[..n].to_vec();
            out.extend(midpoint(rest_a, Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().map(|&c| digit(c)).unwrap_or(0);
    let db = b.map(|b| digit(b[0])).unwrap_or(DIGITS.len());
    if db - da > 1 {
        vec![DIGITS[(da + db).div_ceil(2)]]
    } else if let Some(b) = b.filter(|b| b.len() > 1) {
        vec![b[0]]
    } else {
        let rest_a = if a.is_empty() { &[][..] } else { &a[1..] };
        let mut out = vec![DIGITS[da]];
        out.extend(midpoint(rest_a, None));
        out
    }
}
