"""Offline adversarial tests for the standalone installer; no release keys or network.

The ephemeral signing key is substituted only inside the imported Python module.
There is deliberately no signer/URL override in the shipped installer CLI.
"""
import contextlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[2] / "scripts" / "install.sh"
BODY = SOURCE.read_text().split("<<'RDY_INSTALLER_PYTHON'\n", 1)[1].rsplit("\nRDY_INSTALLER_PYTHON", 1)[0]
installer = types.ModuleType("rdy_installer")
exec(compile(BODY, str(SOURCE), "exec"), installer.__dict__)
TARGET = installer.TARGETS[0]


class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if shutil.which("ssh-keygen") is None:
            raise unittest.SkipTest("OpenSSH required for real offline signature fixtures")
        cls.keys = tempfile.TemporaryDirectory(prefix="rdy-installer-key-")
        cls.key = Path(cls.keys.name) / "test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(cls.key)], check=True)
        public = cls.key.with_suffix(".pub").read_text().split()
        cls.signer = 'rdy-release namespaces="rdy-release-v1" ' + " ".join(public[:2])

    @classmethod
    def tearDownClass(cls):
        cls.keys.cleanup()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rdy-installer-test-")
        # macOS /var is a system alias; pass its real path so the installer can
        # reject every symlink in a user-supplied installation prefix.
        self.root = Path(self.temporary.name).resolve()
        self.prefix = self.root / "managed"
        self.packet = self.root / "packet"
        self.packet.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def archive(self, version="0.17.0", target=TARGET, extra=(), binary=None, omit=()):
        name = installer.archive_name(version, target)
        stem = name[:-7]
        if binary is None:
            binary = ("#!/bin/sh\nprintf '%s\\n' 'rdy " + version + " (preview)'\n").encode()
        files = {"rdy": binary, "LICENSE-MIT": b"MIT\n", "LICENSE-APACHE": b"Apache\n", "PREVIEW.txt": b"Preview\n"}
        path = self.packet / name
        with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as out:
            root = tarfile.TarInfo(stem)
            root.type = tarfile.DIRTYPE
            root.mode = 0o755
            out.addfile(root)
            for relative, data in files.items():
                if relative in omit:
                    continue
                member = tarfile.TarInfo(stem + "/" + relative)
                member.size = len(data)
                member.mode = 0o755 if relative == "rdy" else 0o644
                out.addfile(member, io.BytesIO(data))
            for member, data in extra:
                out.addfile(member, io.BytesIO(data) if data is not None else None)
        return path

    def signed_packet(self, version="0.17.0", **kwargs):
        for target in installer.TARGETS:
            self.archive(version, target=target, **kwargs)
        manifest = self.packet / "SHA256SUMS"
        manifest.write_text("".join(installer.digest(self.packet / installer.archive_name(version, target)) + "  " + installer.archive_name(version, target) + "\n" for target in installer.TARGETS))
        self.sign(manifest)

    def sign(self, manifest, namespace="rdy-release-v1"):
        Path(str(manifest) + ".sig").unlink(missing_ok=True)
        subprocess.run(["ssh-keygen", "-Y", "sign", "-f", str(self.key), "-n", namespace, str(manifest)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def run_main(self, *args):
        with patch.object(installer, "SIGNER", self.signer), patch.object(installer, "platform_target", return_value=TARGET), contextlib.redirect_stdout(io.StringIO()):
            installer.main(["--prefix", str(self.prefix), *args])

    def install(self, version="0.17.0"):
        self.run_main("--version", version, "--offline-dir", str(self.packet))

    def test_offline_install_upgrade_activate_and_uninstall(self):
        self.signed_packet()
        self.install()
        command = self.prefix / "bin/rdy"
        self.assertTrue(command.is_symlink())
        self.assertEqual(subprocess.check_output([str(command), "--version"]), b"rdy 0.17.0 (preview)\n")
        original = os.readlink(command)
        self.signed_packet("0.18.0")
        self.install("0.18.0")
        self.assertNotEqual(os.readlink(command), original)
        self.run_main("--activate", "0.17.0")
        self.assertEqual(os.readlink(command), original)
        self.run_main("--uninstall")
        self.assertFalse(command.exists())
        self.assertTrue((self.prefix / "share/rdy/versions" / ("0.17.0--" + TARGET)).is_dir())

    def test_tampered_archive_is_never_executed(self):
        self.signed_packet()
        archive = self.packet / installer.archive_name("0.17.0", TARGET)
        archive.write_bytes(archive.read_bytes() + b"tampered")
        with patch.object(installer, "version_probe") as execute:
            with self.assertRaisesRegex(installer.Refusal, "checksum mismatch"):
                self.install()
            execute.assert_not_called()
        self.assertFalse((self.prefix / "bin/rdy").exists())

    def test_wrong_signature_namespace_is_refused(self):
        self.signed_packet()
        self.sign(self.packet / "SHA256SUMS", namespace="wrong-purpose")
        with self.assertRaisesRegex(installer.Refusal, "signature did not verify"):
            self.install()

    def test_untrusted_signer_is_refused(self):
        self.signed_packet()
        with self.assertRaisesRegex(installer.Refusal, "signature did not verify"):
            installer.verify_release(self.packet, "0.17.0", TARGET)

    def test_tampered_signature_is_refused(self):
        self.signed_packet()
        (self.packet / "SHA256SUMS.sig").write_bytes(b"not an SSH signature\n")
        with self.assertRaisesRegex(installer.Refusal, "signature did not verify"):
            self.install()

    def test_exact_manifest_grammar(self):
        self.signed_packet()
        valid = (self.packet / "SHA256SUMS").read_bytes()
        for changed in (valid + b"extra\n", valid.replace(b"\n", b"\r\n"), valid.rstrip(b"\n"), valid.splitlines(keepends=True)[0] * 2, valid.replace(b"  rdy", b" ../rdy")):
            with self.subTest(changed=changed[:30]), self.assertRaises(installer.Refusal):
                installer.checksums(changed, "0.17.0")

    def test_safe_archive_shape(self):
        archive = self.archive()
        stage = self.root / "stage"
        stage.mkdir()
        installer.unpack(archive, stage, "0.17.0", TARGET)
        self.assertEqual({p.name for p in stage.iterdir()}, {"rdy", "LICENSE-MIT", "LICENSE-APACHE", "PREVIEW.txt"})

    def test_archive_rejects_traversal_links_devices_and_duplicates(self):
        stem = installer.archive_name("0.17.0", TARGET)[:-7]
        for kind in ("traversal", "absolute", "symlink", "hardlink", "device", "duplicate", "setuid"):
            with self.subTest(kind=kind):
                name = {"traversal": stem + "/../../escaped", "absolute": "/tmp/escaped", "duplicate": stem + "/rdy"}.get(kind, stem + "/bad")
                member = tarfile.TarInfo(name)
                member.mode = 0o4755 if kind == "setuid" else 0o644
                member.type = {"symlink": tarfile.SYMTYPE, "hardlink": tarfile.LNKTYPE, "device": tarfile.CHRTYPE}.get(kind, tarfile.REGTYPE)
                member.linkname = "../../escaped" if kind in ("symlink", "hardlink") else ""
                archive = self.archive(extra=[(member, b"" if member.isreg() else None)])
                stage = self.root / ("stage-" + kind)
                stage.mkdir()
                with self.assertRaises(installer.Refusal):
                    installer.unpack(archive, stage, "0.17.0", TARGET)
                self.assertFalse((self.root / "escaped").exists())

    def test_missing_member_and_expansion_limit(self):
        stage = self.root / "stage"
        stage.mkdir()
        with self.assertRaisesRegex(installer.Refusal, "lacks its exact"):
            installer.unpack(self.archive(omit=["LICENSE-MIT"]), stage, "0.17.0", TARGET)
        stage2 = self.root / "stage2"
        stage2.mkdir()
        with patch.object(installer, "EXPANDED_MAX", 512), self.assertRaisesRegex(installer.Refusal, "expanded archive"):
            installer.unpack(self.archive(), stage2, "0.17.0", TARGET)

    def test_bad_version_probe_preserves_existing_installation(self):
        self.signed_packet()
        self.install()
        previous = os.readlink(self.prefix / "bin/rdy")
        self.signed_packet("0.18.0", binary=b"#!/bin/sh\nprintf 'wrong version\\n'\n")
        with self.assertRaisesRegex(installer.Refusal, "unexpected version"):
            self.install("0.18.0")
        self.assertEqual(os.readlink(self.prefix / "bin/rdy"), previous)

    def test_version_output_is_bounded(self):
        self.signed_packet(binary=b"#!/bin/sh\nwhile :; do printf 'too much output\\n'; done\n")
        with self.assertRaises(installer.Refusal):
            self.install()
        self.assertFalse((self.prefix / "bin/rdy").exists())

    def test_unmanaged_command_is_not_overwritten(self):
        (self.prefix / "bin").mkdir(parents=True)
        command = self.prefix / "bin/rdy"
        command.write_text("existing user executable")
        self.signed_packet()
        with self.assertRaisesRegex(installer.Refusal, "unmanaged"):
            self.install()
        self.assertEqual(command.read_text(), "existing user executable")

    def test_external_command_link_is_not_overwritten(self):
        (self.prefix / "bin").mkdir(parents=True)
        (self.prefix / "bin/rdy").symlink_to("/usr/bin/true")
        self.signed_packet()
        with self.assertRaisesRegex(installer.Refusal, "outside this installer"):
            self.install()
        self.assertEqual(os.readlink(self.prefix / "bin/rdy"), "/usr/bin/true")

    def test_symlink_prefix_is_refused(self):
        real = self.root / "real"
        real.mkdir()
        self.prefix.symlink_to(real)
        self.signed_packet()
        with self.assertRaisesRegex(installer.Refusal, "must not be symlinks"):
            self.install()

    def test_other_writable_prefix_is_refused(self):
        self.prefix.mkdir()
        self.prefix.chmod(0o777)
        self.signed_packet()
        with self.assertRaisesRegex(installer.Refusal, "writable by another"):
            self.install()

    def test_binary_tamper_prevents_activation_and_uninstall(self):
        self.signed_packet()
        self.install()
        command = self.prefix / "bin/rdy"
        command.write_text("tampered")
        for args in (("--activate", "0.17.0"), ("--uninstall",)):
            with self.subTest(args=args), self.assertRaisesRegex(installer.Refusal, "changed since installation"):
                self.run_main(*args)
        self.assertTrue(command.is_symlink())

    def test_invalid_receipt_refuses_without_type_error(self):
        self.signed_packet()
        self.install()
        version = self.prefix / "share/rdy/versions" / ("0.17.0--" + TARGET)
        (version / "receipt.json").write_text("null\n")
        with self.assertRaises(installer.Refusal):
            self.run_main("--activate", "0.17.0")

    def test_concurrent_install_lock(self):
        with installer.install_lock(self.root):
            with self.assertRaisesRegex(installer.Refusal, "another installer"):
                with installer.install_lock(self.root):
                    pass

    def test_offline_symlink_input_is_refused(self):
        self.signed_packet()
        source = self.packet / "SHA256SUMS"
        renamed = self.packet / "original"
        source.rename(renamed)
        source.symlink_to(renamed)
        with self.assertRaises(OSError):
            self.install()

    def test_fifo_input_is_refused_without_waiting_for_writer(self):
        source = self.packet / "fifo"
        os.mkfifo(source)
        with self.assertRaisesRegex(installer.Refusal, "regular file"):
            installer.read_regular(source, 100)
        with self.assertRaisesRegex(installer.Refusal, "regular file"):
            installer.copy_regular(source, self.root / "copy", 100)

    def test_unsupported_platforms_and_musl(self):
        for system, machine in (("Windows", "AMD64"), ("Linux", "aarch64"), ("FreeBSD", "x86_64")):
            with self.subTest(system=system), self.assertRaises(installer.Refusal):
                installer.platform_target(system, machine)
        with patch.object(installer.os, "confstr", return_value=None), self.assertRaisesRegex(installer.Refusal, "GNU libc"):
            installer.platform_target("Linux", "x86_64")
        with patch.object(installer.os, "confstr", return_value="glibc 2.39"):
            self.assertEqual(installer.platform_target("Linux", "x86_64"), installer.TARGETS[1])

    def test_url_allowlist_and_redirect_downgrade(self):
        for url in ("http://github.com/a", "https://github.com.attacker.test/a", "https://user:password@github.com/a", "https://example.com/a", "https://github.com:444/a", "https://github.com/a#fragment"):
            with self.subTest(url=url), self.assertRaises(installer.Refusal):
                installer.trusted_url(url)
        handler = installer.Redirects()
        with self.assertRaises(installer.Refusal):
            handler.redirect_request(None, None, 302, "", {}, "http://github.com/a")

    def test_download_rejects_truncation_and_oversize(self):
        class Response(io.BytesIO):
            status = 200
            url = "https://github.com/adrenickchrome/rdy-core/releases/download/v0.17.0/SHA256SUMS"

        for body, declared, limit in ((b"short", "10", 100), (b"12345", None, 4), (b"a", "1000", 100)):
            with self.subTest(declared=declared, limit=limit):
                response = Response(body)
                response.headers = {} if declared is None else {"Content-Length": declared}
                opener = types.SimpleNamespace(open=lambda *args, **kwargs: response)
                destination = self.root / ("download-" + str(declared) + "-" + str(limit))
                with patch.object(installer.urllib.request, "build_opener", return_value=opener), self.assertRaises(installer.Refusal):
                    installer.download(Response.url, destination, limit)

    def release_row(self, minor, **overrides):
        version = "0." + str(minor) + ".0"
        row = {"id": minor, "tag_name": "v" + version, "prerelease": True, "draft": False,
               "assets": [{"name": name} for name in ["SHA256SUMS", "SHA256SUMS.sig"] + [installer.archive_name(version, target) for target in installer.TARGETS]]}
        row.update(overrides)
        return row

    def test_preview_channel_ignores_draft_stable_and_unrelated_tags(self):
        rows = [self.release_row(9), self.release_row(17), self.release_row(18, draft=True), self.release_row(19, prerelease=False), self.release_row(20, tag_name="v1.0.0")]
        with patch.object(installer, "metadata", return_value=rows):
            self.assertEqual(installer.resolve_release(None, "preview", self.root), ("0.17.0", 17))

    def test_incomplete_or_mismatched_release_is_refused(self):
        for row in (self.release_row(17, assets=[]), self.release_row(18), self.release_row(17, draft=True)):
            with patch.object(installer, "metadata", return_value=row), self.assertRaises(installer.Refusal):
                installer.resolve_release("0.17.0", None, self.root)

    def test_cli_exposes_no_trust_or_download_override(self):
        result = subprocess.run(["/bin/sh", str(SOURCE), "--help"], capture_output=True, text=True, check=True)
        self.assertIn("--offline-dir", result.stdout)
        for forbidden in ("--skip", "--signer", "--url", "--key"):
            self.assertNotIn(forbidden, result.stdout)

    def test_missing_python_has_a_clear_error(self):
        empty_path = self.root / "empty-path"
        empty_path.mkdir()
        result = subprocess.run(["/bin/sh", str(SOURCE), "--help"], capture_output=True, text=True, env={"PATH": str(empty_path)})
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires Python 3.9 or later", result.stderr)


if __name__ == "__main__":
    unittest.main()
