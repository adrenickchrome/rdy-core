"""Release refusal tests use disposable data and mocked GitHub only; no release writes."""
import base64
import copy
import datetime as dt
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import urllib.error
import zipfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_control', ROOT / 'scripts/release/control.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
SHA = 'a' * 40
VERSION = '0.18.0'


def factory():
    machine = next(iter(c.read_json(ROOT / 'crates/rdy-qa/qa-baseline.json')['classes']))
    gate = {'result': 'pass', 'command': 'cargo qa --deep', 'evidence_sha256': 'b' * 64, 'evidence_uri': 'https://example.org/factory/receipt.json', 'machine_class': machine}
    return {'schema': 1, 'source_sha': SHA, 'agent_guide_sha256': c.sha(ROOT / 'scripts/release/agent-guide.md'), 'deep': gate.copy(), 'performance': gate.copy()}


def archive(path, ver=VERSION, target=c.TARGETS[0], attack=None):
    stem = c.archive_name(ver, target)[:-7]
    with tarfile.open(path, 'w:gz') as output:
        for name in ('rdy', 'LICENSE-MIT', 'LICENSE-APACHE', 'PREVIEW.txt'):
            payload = b'preview fixture' if name == 'PREVIEW.txt' else b'fixture'
            item = tarfile.TarInfo(stem + '/' + name)
            item.size = len(payload)
            item.mode = 0o755 if name == 'rdy' else 0o644
            output.addfile(item, io.BytesIO(payload))
        if attack:
            output.addfile(attack, io.BytesIO(b'x' * attack.size))


class ArchiveTests(unittest.TestCase):
    def test_safe_native_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'safe.tar.gz'
            archive(path)
            c.inspect_archive(path, VERSION, c.TARGETS[0])

    def test_traversal_link_device_duplicate_and_extra_refuse(self):
        attacks = []
        for name in ('../outside', '/absolute', 'extra', c.archive_name(VERSION, c.TARGETS[0])[:-7] + '/rdy'):
            a = tarfile.TarInfo(name); a.size = 1; attacks.append(a)
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE):
            a = tarfile.TarInfo('link'); a.type = kind; a.linkname = '../outside'; attacks.append(a)
        for attack in attacks:
            with self.subTest(member=attack.name, kind=attack.type), tempfile.TemporaryDirectory() as temp:
                path = Path(temp) / 'attack.tar.gz'; archive(path, attack=attack)
                with self.assertRaises(c.Refusal):
                    c.inspect_archive(path, VERSION, c.TARGETS[0])

    def test_decompression_cap_applies_before_tar_metadata(self):
        reader = c.BoundedArchiveReader(io.BytesIO(b'x' * 100), 50)
        self.assertEqual(len(reader.read(30)), 30)
        with self.assertRaises(c.Refusal): reader.read(10**9)

    def test_missing_preview_and_oversize_refuse(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'empty.tar.gz'
            with tarfile.open(path, 'w:gz'):
                pass
            with self.assertRaises(c.Refusal):
                c.inspect_archive(path, VERSION, c.TARGETS[0])
            archive(path)
            with patch.object(c, 'MAX_FILE', 2), self.assertRaises(c.Refusal):
                c.inspect_archive(path, VERSION, c.TARGETS[0])


class IdentityTests(unittest.TestCase):
    def test_version_grammar(self):
        for value in ('v0.18.0', '1.0.0', '0.018.0', '0.18.1', '0.18.0;echo', '../0.18.0', '0.18.0\n'):
            with self.subTest(value=value), self.assertRaises(c.Refusal): c.version(value)
        self.assertEqual(c.version(VERSION), VERSION)

    def test_public_signature_encoding_bound(self):
        for value in ('', 'x' * 6000, '$(echo)', 'AAA\nAA', base64.b64encode(b'private key').decode()):
            with self.subTest(value=value[:10]), patch.object(c, 'verify_packet', return_value={'version': VERSION}), self.assertRaises((c.Refusal, ValueError)):
                c.seal('/nonexistent', '10', 'b' * 64, value)

    def test_factory_source_fail_missing_and_machine_baseline(self):
        c.validate_factory(factory(), SHA)
        cases = []
        for key, value in [('result', 'skipped'), ('evidence_sha256', 'unknown'), ('machine_class', 'unqualified-host'), ('evidence_uri', 'https://example.org/log?token=private')]:
            f = factory(); f['performance'][key] = value; cases.append(f)
        f = factory(); f['source_sha'] = 'c' * 40; cases.append(f)
        f = factory(); del f['deep']; cases.append(f)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(c.Refusal): c.validate_factory(value, SHA)

    def test_duplicate_json_key_refuses(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp) / 'record'; p.write_text('{"sha":"a","sha":"b"}')
            with self.assertRaises(c.Refusal): c.read_json(p)

    def test_required_checks_path_sha_event_and_aggregate(self):
        def response(path, *args, **kwargs):
            workflow = next((w for w in c.REQUIRED if '/' + w + '/' in path), None)
            if workflow:
                return {'workflow_runs': [{'id': 1, 'run_attempt': 1, 'head_sha': SHA, 'head_branch': 'main', 'event': 'push', 'path': '.github/workflows/' + workflow, 'status': 'completed', 'conclusion': 'success'}]}
            return {'total_count': 3, 'jobs': [{'id': i, 'name': name, 'status': 'completed', 'conclusion': 'success'} for i, name in enumerate(c.REQUIRED.values())]}
        with patch.object(c, 'api', side_effect=response): self.assertEqual(len(c.required_checks(SHA)), 3)
        for change in ({'head_sha': 'b' * 40}, {'event': 'pull_request'}, {'head_branch': 'topic'}, {'status': 'in_progress'}, {'conclusion': 'failure'}, {'path': '.github/workflows/fake.yml'}):
            def bad(path, *args, **kwargs):
                data = response(path)
                if 'workflow_runs' in data: data['workflow_runs'][0].update(change)
                return data
            with self.subTest(change=change), patch.object(c, 'api', side_effect=bad), self.assertRaises(c.Refusal): c.required_checks(SHA)
        def skipped(path, *args, **kwargs):
            data = response(path)
            if 'jobs' in data:
                for job in data['jobs']: job['conclusion'] = 'skipped'
            return data
        with patch.object(c, 'api', side_effect=skipped), self.assertRaises(c.Refusal): c.required_checks(SHA)


class PacketTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)
        for target in c.TARGETS:
            archive(self.path / c.archive_name(VERSION, target), target=target)
            c.write_json(self.path / f'build-{target}.json', {'source_sha': SHA, 'control_sha': SHA, 'version': VERSION, 'target': target, 'run_id': 10, 'run_attempt': 1, 'sha256': c.sha(self.path / c.archive_name(VERSION, target))})
            c.write_json(self.path / f'sbom-{target}.cdx.json', {'bomFormat': 'CycloneDX'})
        for name in ('install.sh', 'INSTALL.md', 'AGENTS.md', 'CLAUDE.md'):
            (self.path / name).write_bytes((ROOT / 'scripts/release/agent-guide.md').read_bytes() if name in ('AGENTS.md', 'CLAUDE.md') else b'fixture\n')
        c.write_json(self.path / 'admission.json', {'source_sha': SHA, 'version': VERSION, 'factory_qualification': factory()})
        (self.path / 'SHA256SUMS').write_text(''.join(f'{c.sha(self.path / c.archive_name(VERSION,t))}  {c.archive_name(VERSION,t)}\n' for t in c.TARGETS))
        self.manifest = c.sha(self.path / 'SHA256SUMS')
        record = {'schema': 1, 'repository': c.REPO, 'repository_id': c.REPO_ID, 'source_sha': SHA, 'control_sha': SHA, 'version': VERSION, 'run_id': 10, 'run_attempt': 1, 'created_at': c.now().isoformat(), 'expires_at': (c.now() + dt.timedelta(days=7)).isoformat(), 'files': {p.name: {'sha256': c.sha(p), 'bytes': p.stat().st_size} for p in self.path.iterdir()}}
        c.write_json(self.path / 'candidate.json', record)
        (self.path / 'provenance.sigstore.json').write_text('fixture')
        self.record = record
        self.env = patch.dict(os.environ, {'GITHUB_SHA': SHA}); self.env.start(); self.addCleanup(self.env.stop)
        # Only source guide lookup and external cryptographic verifier are mocked; packet and archive validation are real.
        original_sha = c.sha
        self.sha_patch = patch.object(c, 'sha', side_effect=lambda p: original_sha(self.path / {'scripts/install.sh': 'install.sh', 'docs/install.md': 'INSTALL.md', 'scripts/release/agent-guide.md': 'AGENTS.md'}[str(Path(p).relative_to(ROOT))]) if Path(p).is_relative_to(ROOT) and str(Path(p).relative_to(ROOT)) in {'scripts/install.sh', 'docs/install.md', 'scripts/release/agent-guide.md'} else original_sha(p))
        self.sha_patch.start(); self.addCleanup(self.sha_patch.stop)
        self.proof = patch.object(c, 'verify_provenance'); self.proof.start(); self.addCleanup(self.proof.stop)

    def test_good_packet(self):
        self.assertEqual(c.verify_packet(self.path, '10', self.manifest)['version'], VERSION)

    def test_wrong_source_run_attempt_expired_repository_refuse(self):
        for key, value in [('source_sha', 'f' * 40), ('control_sha', 'f' * 40), ('run_id', 11), ('run_attempt', 2), ('repository_id', 42), ('repository', 'evil/repo'), ('expires_at', '2001-01-01T00:00:00+00:00'), ('created_at', '2099-01-01T00:00:00+00:00')]:
            changed = copy.deepcopy(self.record); changed[key] = value; c.write_json(self.path / 'candidate.json', changed)
            with self.subTest(key=key), self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', self.manifest)

    def test_extra_asset_and_changed_bytes_refuse(self):
        (self.path / 'extra.sh').write_text('anything')
        with self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', self.manifest)
        (self.path / 'extra.sh').unlink()
        (self.path / 'install.sh').write_text('changed')
        with self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', self.manifest)

    def test_signature_manifest_binding_refuses(self):
        with self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', 'b' * 64)

    def test_symlink_packet_refuses(self):
        (self.path / 'install.sh').unlink(); (self.path / 'install.sh').symlink_to('/etc/passwd')
        with self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', self.manifest)

    def test_candidate_symlink_refuses_before_read(self):
        (self.path / 'candidate.json').unlink(); (self.path / 'candidate.json').symlink_to('/etc/passwd')
        with self.assertRaises(c.Refusal): c.verify_packet(self.path, '10', self.manifest)

    def test_release_assets_exact_and_digest_reconciliation(self):
        assets = [{'name': p.name, 'state': 'uploaded', 'size': p.stat().st_size, 'digest': 'sha256:' + c.sha(p)} for p in self.path.iterdir()]
        c.release_assets_match({'assets': assets}, self.path)
        for changed in ([*assets, assets[0]], assets[:-1], [{**assets[0], 'digest': 'sha256:' + '0' * 64}, *assets[1:]], [{**assets[0], 'name': 'foreign'}, *assets[1:]]):
            with self.subTest(changed=changed[0]['name']), self.assertRaises(c.Refusal): c.release_assets_match({'assets': changed}, self.path)

    def test_publisher_disabled_before_any_mutation(self):
        with patch.object(c, 'identity', return_value=SHA), patch.object(c, 'api') as api, patch.dict(os.environ, {'CORE_RELEASE_CONTROLS_READY': ''}), self.assertRaises(c.Refusal):
            c.publish(self.path, '10', self.manifest)
        api.assert_not_called()


class PublisherTests(unittest.TestCase):
    def setUp(self):
        PacketTests.setUp(self)
        (self.path / 'SHA256SUMS.sig').write_text('fixture public signature')
        for name, value in [('identity', SHA), ('candidate_run', {}), ('required_checks', {}), ('verify_signature', None), ('output', None)]:
            item = patch.object(c, name, return_value=value); item.start(); self.addCleanup(item.stop)
        env = patch.dict(os.environ, {'CORE_RELEASE_CONTROLS_READY': 'true', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_RUN_ID': '99'})
        env.start(); self.addCleanup(env.stop)
        receipt_temp = tempfile.TemporaryDirectory(); self.addCleanup(receipt_temp.cleanup)
        self.receipts = Path(receipt_temp.name)
        for target in c.TARGETS:
            c.write_json(self.receipts / f'acceptance-{target}.json', {'candidate_run_id': 10, 'source_sha': SHA, 'manifest_sha256': self.manifest, 'target': target, 'result': 'passed', 'managed_installer': 'passed', 'existing_checker_report': {'unpack': '/tmp/original-run-99'}})
        self.release = None
        self.writes = []
        self.foreign_tag = False
        self.downloads = {}
        self.prior_artifacts = []
        self.prior_run = {'repository': {'id': c.REPO_ID}, 'head_repository': {'id': c.REPO_ID}, 'head_sha': SHA, 'head_branch': 'main', 'event': 'workflow_dispatch', 'path': '.github/workflows/release-publish.yml', 'status': 'completed', 'conclusion': 'failure', 'run_attempt': 1}
        self.prior_jobs = [{'name': f'Signed native install ({t})', 'status': 'completed', 'conclusion': 'success'} for t in c.TARGETS]
        downloader = patch.object(c, 'receipt_bytes', side_effect=self.fake_download)
        downloader.start(); self.addCleanup(downloader.stop)

    def fake_download(self, endpoint, size, digest):
        blob = self.downloads[endpoint]
        c.require(len(blob) == size and 'sha256:' + c.hashlib.sha256(blob).hexdigest() == digest, 'fixture download digest mismatch')
        return blob

    def fresh_dispatch(self):
        (self.path / 'release-qualification.json').unlink()
        os.environ['GITHUB_RUN_ID'] = '100'
        for target in c.TARGETS:
            path = self.receipts / f'acceptance-{target}.json'
            report = c.read_json(path)
            report['existing_checker_report']['unpack'] = '/private/tmp/fresh-run-100'
            c.write_json(path, report)

    def fake_api(self, path, method='GET', data=None, absent=False, upload=None):
        if method != 'GET': self.writes.append((method, path))
        if '/actions/runs/' in path:
            if '/jobs?' in path: return {'total_count': len(self.prior_jobs), 'jobs': self.prior_jobs}
            if '/artifacts?' in path: return {'total_count': len(self.prior_artifacts), 'artifacts': self.prior_artifacts}
            return self.prior_run
        if '/git/ref/' in path:
            return {'object': {'type': 'commit', 'sha': 'f' * 40}} if self.foreign_tag else None
        if method == 'POST' and path.endswith('/releases'):
            self.release = {**data, 'id': 42, 'assets': [], 'immutable': False}
            return copy.deepcopy(self.release)
        if method == 'POST' and '/assets?' in path:
            name = Path(upload).name
            asset_id = len(self.release['assets']) + 200
            self.release['assets'].append({'id': asset_id, 'name': name, 'state': 'uploaded', 'size': Path(upload).stat().st_size, 'digest': 'sha256:' + c.sha(upload)})
            self.downloads[f'/repos/{c.REPO}/releases/assets/{asset_id}'] = Path(upload).read_bytes()
            if name == 'release-qualification.json':
                for i, target in enumerate(c.TARGETS):
                    buf = io.BytesIO()
                    with zipfile.ZipFile(buf, 'w') as archive:
                        receipt = self.receipts / f'acceptance-{target}.json'
                        archive.writestr(receipt.name, receipt.read_bytes())
                    blob = buf.getvalue(); artifact_id = 900 + i
                    self.downloads[f'/repos/{c.REPO}/actions/artifacts/{artifact_id}/zip'] = blob
                    self.prior_artifacts.append({'id': artifact_id, 'name': 'signed-install-' + target, 'expired': False, 'size_in_bytes': len(blob), 'digest': 'sha256:' + c.hashlib.sha256(blob).hexdigest()})
            return copy.deepcopy(self.release['assets'][-1])
        if method == 'PATCH':
            self.release.update(data); self.release['immutable'] = True
            return copy.deepcopy(self.release)
        if '?per_page=' in path: return [copy.deepcopy(self.release)] if self.release else []
        return copy.deepcopy(self.release)

    def test_draft_upload_publish_then_exact_idempotent_reconcile(self):
        with patch.object(c, 'api', side_effect=self.fake_api):
            c.publish(self.path, '10', self.manifest, self.receipts)
            self.assertFalse(self.release['draft']); self.assertTrue(self.release['immutable'])
            self.assertEqual(len(self.release['assets']), len(list(self.path.iterdir())))
            writes = len(self.writes)
            c.publish(self.path, '10', self.manifest, self.receipts)
            self.assertEqual(len(self.writes), writes, 'published reconciliation must perform no writes')

    def test_foreign_tag_prevents_any_write(self):
        self.foreign_tag = True
        with patch.object(c, 'api', side_effect=self.fake_api), self.assertRaises(c.Refusal): c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual(self.writes, [])

    def test_existing_asset_mismatch_preserves_release(self):
        with patch.object(c, 'api', side_effect=self.fake_api): c.publish(self.path, '10', self.manifest, self.receipts)
        self.release['assets'][0]['digest'] = 'sha256:' + '0' * 64
        self.writes.clear()
        with patch.object(c, 'api', side_effect=self.fake_api), self.assertRaises(c.Refusal): c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual(self.writes, [])

    def test_uncertain_create_never_blindly_retries(self):
        def failing(path, method='GET', data=None, **kwargs):
            if method == 'POST':
                self.writes.append((method, path))
                raise c.Refusal('uncertain network result')
            return self.fake_api(path, method, data=data, **kwargs)
        with patch.object(c, 'api', side_effect=failing), self.assertRaises(c.Refusal): c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual(len(self.writes), 1)

    def test_partial_matching_draft_only_uploads_missing_assets(self):
        with patch.object(c, 'api', side_effect=self.fake_api): c.publish(self.path, '10', self.manifest, self.receipts)
        self.release['draft'] = True; self.release['immutable'] = False
        self.release['assets'].pop(); self.writes.clear()
        with patch.object(c, 'api', side_effect=self.fake_api): c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual(sum(method == 'POST' for method, path in self.writes), 1)
        self.assertEqual(sum(method == 'PATCH' for method, path in self.writes), 1)

    def test_fresh_dispatch_retains_original_immutable_publication_receipt(self):
        with patch.object(c, 'api', side_effect=self.fake_api):
            c.publish(self.path, '10', self.manifest, self.receipts)
            original = (self.path / 'release-qualification.json').read_bytes()
            self.fresh_dispatch(); self.writes.clear()
            c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual((self.path / 'release-qualification.json').read_bytes(), original)
        self.assertEqual(c.read_json(self.path / 'release-qualification.json')['finalization_run_id'], 99)
        self.assertEqual(self.writes, [], 'new dispatch must not rewrite a published release')

    def test_fresh_dispatch_resumes_partial_draft_after_original_qualification_upload(self):
        with patch.object(c, 'api', side_effect=self.fake_api):
            c.publish(self.path, '10', self.manifest, self.receipts)
            original = (self.path / 'release-qualification.json').read_bytes()
            self.release['draft'] = True; self.release['immutable'] = False
            self.release['assets'] = [a for a in self.release['assets'] if a['name'] != 'install.sh']
            self.fresh_dispatch(); self.writes.clear()
            c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertEqual((self.path / 'release-qualification.json').read_bytes(), original)
        self.assertEqual(sum(m == 'POST' for m, _ in self.writes), 1)
        self.assertEqual(sum(m == 'PATCH' for m, _ in self.writes), 1)
        self.assertTrue(all('release-qualification' not in path for _, path in self.writes))

    def test_prior_failed_job_changed_workflow_rerun_or_expired_receipt_refuses(self):
        with patch.object(c, 'api', side_effect=self.fake_api):
            c.publish(self.path, '10', self.manifest, self.receipts)
            self.fresh_dispatch(); self.writes.clear()
            for container, key, value in [(self.prior_run, 'head_sha', 'f' * 40), (self.prior_run, 'path', '.github/workflows/untrusted.yml'), (self.prior_run, 'run_attempt', 2), (self.prior_jobs[0], 'conclusion', 'failure'), (self.prior_artifacts[0], 'expired', True), (self.prior_artifacts[0], 'digest', 'sha256:' + '0' * 64)]:
                old = container[key]; container[key] = value
                with self.subTest(key=key), self.assertRaises(c.Refusal):
                    c.publish(self.path, '10', self.manifest, self.receipts)
                container[key] = old
                self.assertFalse((self.path / 'release-qualification.json').exists())
                self.assertEqual(self.writes, [])

    def test_prior_receipt_body_must_match_durable_qualification_hash(self):
        with patch.object(c, 'api', side_effect=self.fake_api):
            c.publish(self.path, '10', self.manifest, self.receipts)
            self.fresh_dispatch(); self.writes.clear()
            artifact = self.prior_artifacts[0]
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, 'w') as archive:
                archive.writestr(f'acceptance-{c.TARGETS[0]}.json', b'{"changed":true}')
            blob = buf.getvalue()
            self.downloads[f'/repos/{c.REPO}/actions/artifacts/{artifact["id"]}/zip'] = blob
            artifact['size_in_bytes'] = len(blob); artifact['digest'] = 'sha256:' + c.hashlib.sha256(blob).hexdigest()
            with self.assertRaises(c.Refusal): c.publish(self.path, '10', self.manifest, self.receipts)
            self.assertEqual(self.writes, [])

    def test_missing_immutability_postcondition_fails_without_delete(self):
        def mutable(path, method='GET', data=None, **kwargs):
            out = self.fake_api(path, method, data=data, **kwargs)
            if method == 'PATCH': out['immutable'] = False
            return out
        with patch.object(c, 'api', side_effect=mutable), self.assertRaises(c.Refusal): c.publish(self.path, '10', self.manifest, self.receipts)
        self.assertTrue(all(method != 'DELETE' for method, path in self.writes))


class ReceiptDownloadTests(unittest.TestCase):
    def test_storage_redirect_drops_bearer_and_checks_digest(self):
        payload = b'bounded public receipt'
        requests = []
        class Opener:
            def open(self, request, timeout):
                requests.append(request)
                if len(requests) == 1:
                    raise urllib.error.HTTPError(request.full_url, 302, 'redirect', {'Location': 'https://release-assets.githubusercontent.com/exact-object'}, None)
                return io.BytesIO(payload)
        with patch.dict(os.environ, {'GH_TOKEN': 'test-fixture-not-a-secret'}), patch.object(c.urllib.request, 'build_opener', return_value=Opener()):
            self.assertEqual(c.receipt_bytes(f'/repos/{c.REPO}/releases/assets/42', len(payload), 'sha256:' + c.hashlib.sha256(payload).hexdigest()), payload)
        self.assertEqual(requests[0].get_header('Authorization'), 'Bearer test-fixture-not-a-secret')
        self.assertIsNone(requests[1].get_header('Authorization'))

    def test_foreign_redirect_and_wrong_digest_refuse(self):
        class Foreign:
            def open(self, request, timeout):
                raise urllib.error.HTTPError(request.full_url, 302, 'redirect', {'Location': 'https://attacker.example/receipt'}, None)
        with patch.object(c.urllib.request, 'build_opener', return_value=Foreign()), self.assertRaises(c.Refusal):
            c.receipt_bytes(f'/repos/{c.REPO}/releases/assets/42', 3, 'sha256:' + '0' * 64)
        class Changed:
            def open(self, request, timeout): return io.BytesIO(b'changed')
        with patch.object(c.urllib.request, 'build_opener', return_value=Changed()), self.assertRaises(c.Refusal):
            c.receipt_bytes(f'/repos/{c.REPO}/releases/assets/42', 3, 'sha256:' + '0' * 64)


class RealSignatureTests(unittest.TestCase):
    def test_openssh_valid_and_tampered_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp); key = p / 'test-key'
            subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(key)], check=True)
            (p / 'SHA256SUMS').write_text('fixture\n')
            subprocess.run(['ssh-keygen', '-Y', 'sign', '-f', str(key), '-n', 'rdy-release-v1', str(p / 'SHA256SUMS')], check=True, capture_output=True)
            signer = 'rdy-release namespaces="rdy-release-v1" ' + (p / 'test-key.pub').read_text().strip()
            with patch.object(c, 'SIGNER', signer):
                c.verify_signature(p)
                (p / 'SHA256SUMS').write_text('tampered\n')
                with self.assertRaises(c.Refusal): c.verify_signature(p)
            with self.assertRaises(c.Refusal): c.verify_signature(p)


if __name__ == '__main__': unittest.main()
