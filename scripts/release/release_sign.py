#!/usr/bin/env python3
"""Produce a native rdy preview SHA256SUMS.sig from a protected release job.

Input pin, version and manifest digest MUST come from separate trusted policy /
qualification receipts, never from the untrusted candidate directory. No fallback
to an ambient SSH signer. Requires Python 3.10+, AWS CLI v2, and OpenSSH.
"""
import base64
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import tempfile
import textwrap

NS = b"rdy-release-v1"
PRINCIPAL = "rdy-release"
KEY = b"ssh-ed25519"
ALG = "ED25519_SHA_512"
SPEC = "ECC_NIST_EDWARDS25519"
TARGETS = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
ARN = re.compile(r"arn:aws:kms:(us-[a-z]+-[1-9][0-9]*):[0-9]{12}:key/[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
DER_PREFIX = bytes.fromhex("302a300506032b6570032100")  # RFC 8410: id-Ed25519 (absent parameters), 32 octets.
LIMIT = 512 * 1024 * 1024
LAST_KNOWN_HARDWARE_MINOR = 35  # A minimum bound, never a substitute for the actual signed cutover.


class Refused(Exception):
    pass


def require(ok, why):
    if not ok:
        raise Refused(why)


def field(data):
    return struct.pack(">I", len(data)) + data


def pubblob(raw):
    return field(KEY) + field(raw)


def fingerprint(blob):
    return "SHA256:" + base64.b64encode(hashlib.sha256(blob).digest()).decode("ascii").rstrip("=")


def allowed(blob):
    return f'{PRINCIPAL} namespaces="{NS.decode()}" ssh-ed25519 ' + base64.b64encode(blob).decode("ascii")


def preimage(data):
    return b"SSHSIG" + field(NS) + field(b"") + field(b"sha512") + field(hashlib.sha512(data).digest())


def armor(blob, rawsig):
    sig = field(KEY) + field(rawsig)
    envelope = b"SSHSIG" + struct.pack(">I", 1) + field(blob) + field(NS) + field(b"") + field(b"sha512") + field(sig)
    encoded = base64.b64encode(envelope).decode("ascii")
    return ("-----BEGIN SSH SIGNATURE-----\n" + "\n".join(textwrap.wrap(encoded, 76)) + "\n-----END SSH SIGNATURE-----\n").encode("ascii")


class Aws:
    """Only two KMS calls; sends the RAW SSH preimage via stdin, not arguments."""
    def request(self, operation, arn, payload=None):
        region = ARN.fullmatch(arn).group(1)
        cmd = ["aws", "--region", region, "--no-cli-pager", "--output", "json", "kms", operation, "--key-id", arn]
        if payload is not None:
            cmd += ["--message-type", "RAW", "--signing-algorithm", ALG, "--message", "fileb:///dev/stdin"]
        env = {**os.environ, "AWS_MAX_ATTEMPTS": "1", "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS": "true",
               "AWS_CLI_AUTO_PROMPT": "off"}
        out = subprocess.run(cmd, input=payload or b"", capture_output=True, timeout=45, env=env, check=False)
        require(out.returncode == 0, "KMS request failed; inspect the trusted job/CloudTrail record")
        return json.loads(out.stdout)


@contextmanager
def regular(dfd, name, limit):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dfd)
    with os.fdopen(fd, "rb") as f:
        st = os.fstat(fd)
        require(stat.S_ISREG(st.st_mode) and 0 < st.st_size <= limit, "invalid candidate file")
        yield f


def verify(blob, manifest, sig):
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        (work / "allowed").write_text(allowed(blob) + "\n", encoding="ascii")
        (work / "sig").write_bytes(sig)
        p = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(work / "allowed"), "-I", PRINCIPAL,
                            "-n", NS.decode(), "-s", str(work / "sig")], input=manifest, capture_output=True, timeout=15)
        require(p.returncode == 0, "OpenSSH rejected the returned signature or existing signature")


def sign_preview(client, directory, version, manifest_sha256, arn, pin, first_machine_tag, *, controller):
    """controller.reserve must durably bind the version BEFORE yielding and exclude concurrent
    revocation until exit. Its trusted implementation must independently authenticate release
    receipts, transition and current revocation state; no standalone CLI can bypass this gate.
    """
    require(ARN.fullmatch(arn), "expected immutable single-region US KMS key ARN")
    require(re.fullmatch(r"SHA256:[A-Za-z0-9+/]{43}", pin), "invalid SSH fingerprint")
    require(re.fullmatch(r"[0-9a-f]{64}", manifest_sha256), "invalid manifest digest")
    require(re.fullmatch(r"0\.[1-9][0-9]{0,9}\.0", version), "invalid preview version")
    boundary = re.fullmatch(r"v0\.([1-9][0-9]{0,9})\.0", first_machine_tag)
    require(boundary and int(boundary[1]) > LAST_KNOWN_HARDWARE_MINOR, "invalid or historical machine cutover")
    require(int(version.split(".")[1]) >= int(boundary[1]), "version is before authenticated machine cutover")
    dfd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        mode = os.fstat(dfd)
        require(mode.st_uid == os.getuid() and not mode.st_mode & 0o022, "candidate directory must be owned and not group/world writable")
        with regular(dfd, "SHA256SUMS", 1024) as f:
            data = f.read(1025)
        require(hashlib.sha256(data).hexdigest() == manifest_sha256, "manifest differs from qualified digest")
        names = [f"rdy-{version}-preview-{t}.tar.gz" for t in TARGETS]
        lines = data.splitlines(keepends=True)
        require(len(lines) == 2, "expected native two-archive preview manifest")
        for name, line in zip(names, lines):
            pattern = rb"([a-f0-9]{64})  " + re.escape(name.encode("ascii")) + rb"\n"
            m = re.fullmatch(pattern, line)
            require(m, "manifest filename/format/order outside native preview contract")
            h = hashlib.sha256()
            size = 0
            with regular(dfd, name, LIMIT) as f:
                for chunk in iter(lambda: f.read(1024 * 1024), b""):
                    size += len(chunk)
                    require(size <= LIMIT, "archive grew beyond limit")
                    h.update(chunk)
            require(h.hexdigest().encode("ascii") == m[1], "archive differs from qualified manifest")
        with controller.reserve(repository="adrenickchrome/rdy-core", version=version,
                                manifest_sha256=manifest_sha256, key_arn=arn, fingerprint=pin,
                                first_machine_tag=first_machine_tag):
            key = client.request("get-public-key", arn)
            require(key.get("KeyId") == arn and key.get("KeySpec") == SPEC and key.get("KeyUsage") == "SIGN_VERIFY"
                    and ALG in key.get("SigningAlgorithms", []), "unexpected KMS key, usage or algorithm")
            der = base64.b64decode(key["PublicKey"], validate=True)
            require(len(der) == len(DER_PREFIX) + 32 and der.startswith(DER_PREFIX), "not strict RFC 8410 Ed25519 SPKI")
            blob = pubblob(der[len(DER_PREFIX):])
            require(fingerprint(blob) == pin, "KMS public fingerprint does not match approved pin")
            try:
                with regular(dfd, "SHA256SUMS.sig", 4096) as f:
                    existing = f.read(4097)
            except FileNotFoundError:
                pass
            else:
                verify(blob, data, existing)
                return "already verified"
            reply = client.request("sign", arn, preimage(data))
            require(reply.get("KeyId") == arn and reply.get("SigningAlgorithm") == ALG, "unexpected signer in KMS response")
            rawsig = base64.b64decode(reply["Signature"], validate=True)
            require(len(rawsig) == 64, "invalid Ed25519 signature length")
            sig = armor(blob, rawsig)
            verify(blob, data, sig)
            # Unique temporary leaf on the same directory handle; atomic no-overwrite publication.
            leaf = ".rdy-sig-" + os.urandom(16).hex()
            fd = os.open(leaf, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=dfd)
            try:
                with os.fdopen(fd, "wb") as f:
                    f.write(sig)
                    f.flush()
                    os.fsync(f.fileno())
                os.link(leaf, "SHA256SUMS.sig", src_dir_fd=dfd, dst_dir_fd=dfd, follow_symlinks=False)
            finally:
                os.unlink(leaf, dir_fd=dfd)
            return "signed and verified"
    finally:
        os.close(dfd)


if __name__ == "__main__":
    raise SystemExit("No standalone live signer. Invoke sign_preview from the protected factory controller with its durable authorization gate.")
