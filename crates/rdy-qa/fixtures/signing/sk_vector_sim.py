#!/usr/bin/env python3
"""Standalone producer of the frozen security-key perf vector (Task 10).

Python standard library only. It imports nothing from rdy and shares no code with rdy's
statement builder or its SSHSIG wrapper. It plays the part of a FIDO authenticator and of the
OpenSSH client for one fixed test key: it writes the statement text by hand, builds the
SSHSIG signed data per OpenSSH PROTOCOL.sshsig, makes the authenticator message per
OpenSSH PROTOCOL.u2f, signs it with Ed25519 (RFC 8032, reference algorithm below) and encodes
the `sk-ssh-ed25519@openssh.com` SSHSIG blob.

The seed is a public test value. No token and no private file is used. Ed25519 is
deterministic, so running this script again prints the same bytes that are pinned in
crates/rdy-qa/src/signing.rs. The same blob is accepted by OpenSSH:

    ssh-keygen -Y verify -f signers -I human:qa -n rdy-approval-v1 -s sig < statement

Run:  python3 sk_vector_sim.py            prints the key line, statement and hex blob
"""
import base64
import hashlib
import struct

# ---- Ed25519, RFC 8032 section 6 (reference implementation) -------------------------------
P = 2**255 - 19
Q = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
I = pow(2, (P - 1) // 4, P)


def _xrecover(y):
    xx = (y * y - 1) * pow(D * y * y + 1, P - 2, P)
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P != 0:
        x = (x * I) % P
    if x % 2 != 0:
        x = P - x
    return x


_BY = 4 * pow(5, P - 2, P) % P
_B = (_xrecover(_BY) % P, _BY % P)


def _add(a, b):
    x1, y1 = a
    x2, y2 = b
    t = D * x1 * x2 * y1 * y2
    x3 = (x1 * y2 + x2 * y1) * pow(1 + t, P - 2, P)
    y3 = (y1 * y2 + x1 * x2) * pow(1 - t, P - 2, P)
    return (x3 % P, y3 % P)


def _mul(point, n):
    result = (0, 1)
    while n:
        if n & 1:
            result = _add(result, point)
        point = _add(point, point)
        n >>= 1
    return result


def _encode(point):
    x, y = point
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def _h(data):
    return hashlib.sha512(data).digest()


def _clamp(seed):
    a = bytearray(_h(seed)[:32])
    a[0] &= 248
    a[31] &= 127
    a[31] |= 64
    return int.from_bytes(a, "little")


def public_key(seed):
    return _encode(_mul(_B, _clamp(seed)))


def sign(seed, message):
    a = _clamp(seed)
    prefix = _h(seed)[32:]
    pub = _encode(_mul(_B, a))
    r = int.from_bytes(_h(prefix + message), "little") % Q
    big_r = _encode(_mul(_B, r))
    k = int.from_bytes(_h(big_r + pub + message), "little") % Q
    s = (r + k * a) % Q
    return big_r + int.to_bytes(s, 32, "little")


# ---- SSH wire encoding ---------------------------------------------------------------------
def string(data):
    if isinstance(data, str):
        data = data.encode()
    return struct.pack(">I", len(data)) + data


SEED = bytes([0x5B]) * 32
FLAGS = 0x01  # user presence
COUNTER = 7
APPLICATION = "ssh:"
NAMESPACE = "rdy-approval-v1"
ALG = "sk-ssh-ed25519@openssh.com"


def key_blob():
    return string(ALG) + string(public_key(SEED)) + string(APPLICATION)


def fingerprint():
    digest = hashlib.sha256(key_blob()).digest()
    return "SHA256:" + base64.b64encode(digest).decode().rstrip("=")


def key_line():
    return ALG + " " + base64.b64encode(key_blob()).decode()


def statement():
    # written out by hand: field order and spelling are the statement format, not rdy code
    return (
        "rdy-statement: 1\n"
        "kind: approval\n"
        "repo: " + "14" * 16 + "\n"
        "proposal_version: " + "03" * 32 + "\n"
        "base: none\n"
        "actor: human:qa\n"
        "assurance: local\n"
        "key: " + fingerprint() + "\n"
    ).encode()


def sshsig_blob(message):
    # PROTOCOL.sshsig: the data that is signed
    signed_data = (
        b"SSHSIG"
        + string(NAMESPACE)
        + string("")
        + string("sha512")
        + string(hashlib.sha512(message).digest())
    )
    # PROTOCOL.u2f: what the authenticator signs
    auth_message = (
        hashlib.sha256(APPLICATION.encode()).digest()
        + bytes([FLAGS])
        + struct.pack(">I", COUNTER)
        + hashlib.sha256(signed_data).digest()
    )
    raw = sign(SEED, auth_message)
    signature = string(ALG) + string(raw) + bytes([FLAGS]) + struct.pack(">I", COUNTER)
    return (
        b"SSHSIG"
        + struct.pack(">I", 1)
        + string(key_blob())
        + string(NAMESPACE)
        + string("")
        + string("sha512")
        + string(signature)
    )


if __name__ == "__main__":
    text = statement()
    print("key_line:", key_line())
    print("fingerprint:", fingerprint())
    print("statement:", text.decode().replace("\n", "\\n"))
    print("sig_hex:", sshsig_blob(text).hex())
