const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn idx(c: u8) -> usize {
    DIGITS
        .iter()
        .position(|&d| d == c)
        .expect("order key digit outside base-62")
}

pub fn order_between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("");
    if let Some(b) = b {
        assert!(a < b, "order_between requires a < b, got {a:?} >= {b:?}");
    }
    String::from_utf8(midpoint(a.as_bytes(), b.map(str::as_bytes))).expect("ascii")
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
    let da = a.first().map(|&c| idx(c)).unwrap_or(0);
    let db = b.map(|b| idx(b[0])).unwrap_or(DIGITS.len());
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
