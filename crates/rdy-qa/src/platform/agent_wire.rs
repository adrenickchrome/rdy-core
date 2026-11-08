//! The test SSH agent's protocol: the request bound and the reply to a request. A platform's fake
//! agent reads the bytes from its own endpoint and writes the reply back; nothing here touches an
//! endpoint.

use rdy_engine::signing_crypto::SigningAgent;

/// The largest request packet the test agent reads.
const PACKET_MAX: usize = 262_144;

/// The length of a request's body from its 4-byte prefix; `None` when it is empty or too long, and
/// then no body is read.
pub(super) fn request_len(prefix: [u8; 4]) -> Option<usize> {
    let len = usize::try_from(u32::from_be_bytes(prefix)).ok()?;
    (len != 0 && len <= PACKET_MAX).then_some(len)
}

/// Appends an SSH `string`: a big-endian length, then the bytes.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend(bytes);
}

/// Reads one SSH `string` at `*at` in `body`, every length checked against what remains.
fn take_string<'a>(body: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len_end = at.checked_add(4)?;
    let len = u32::from_be_bytes(body.get(*at..len_end)?.try_into().ok()?);
    let end = len_end.checked_add(usize::try_from(len).ok()?)?;
    let out = body.get(len_end..end)?;
    *at = end;
    Some(out)
}

/// The reply packet to the request `body`, which [`request_len`] bounded: identities and sign
/// requests go to `signer`. Any other request, or a signer error, gets the bare agent failure.
pub(super) fn reply_packet(body: &[u8], signer: &dyn SigningAgent) -> Vec<u8> {
    let failure = vec![5u8];
    let reply = match body.first() {
        Some(11) => match signer.identities() {
            Ok(ids) => {
                let mut out = vec![12u8];
                out.extend(u32::try_from(ids.len()).unwrap_or(u32::MAX).to_be_bytes());
                for id in ids {
                    put_string(&mut out, &id.public_key);
                    put_string(&mut out, b"");
                }
                out
            }
            Err(_) => failure,
        },
        Some(13) => {
            let mut at = 1;
            let key = take_string(body, &mut at);
            let data = take_string(body, &mut at);
            match (key, data) {
                (Some(key), Some(data)) => match signer.sign(key, data) {
                    Ok(sig) => {
                        let mut out = vec![14u8];
                        put_string(&mut out, &sig);
                        out
                    }
                    Err(_) => failure,
                },
                _ => failure,
            }
        }
        _ => failure,
    };
    let mut packet = u32::try_from(reply.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    packet.extend(reply);
    packet
}
