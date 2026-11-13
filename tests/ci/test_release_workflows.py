"""Delivery and boundary pins for every release workflow, and one complete fake release.

Nothing here reaches GitHub, AWS or a key. The notify workflow's own validation script runs unchanged on fake
event files. The release path reuses the fakes of the publication tests: a fake release API, a fake signing
table and fake verifier. The workflow checks read the YAML as text, in the same style as the other CI pins.
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_release_publish as pub  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.github/workflows'
NOTIFY_PATH = WORKFLOWS / 'core-release-notify.yml'
NOTIFY = NOTIFY_PATH.read_text()
RELEASE_FILES = ('release-candidate.yml', 'release-sign.yml', 'release.yml', 'core-release-notify.yml')
REPOSITORY_ID = 1403751388
FULL_NAME = 'adrenickchrome/rdy-core'
SHA40 = r'[0-9a-f]{40}'
LEVEL = {'read': 1, 'write': 2}
S = pub.S


def text_of(name):
    return (WORKFLOWS / name).read_text()


def all_workflows():
    return sorted(p.name for p in WORKFLOWS.glob('*.yml'))


def code_of(text):
    return re.sub(r'(?m)^\s*#.*$', '', text)


def jobs_of(text):
    """Job id -> its block, for a workflow whose jobs sit at two-space indent."""
    body = text[text.index('\njobs:\n') + 1:]
    found = {}
    for match in re.finditer(r'^  ([A-Za-z0-9_-]+):\n((?:(?:    .*|\s*#.*)?\n)+)', body, re.M):
        found[match.group(1)] = match.group(2)
    return found


def job_permissions(block):
    match = re.search(r'^    permissions:\n((?:      .*\n)+)', block, re.M)
    if match is None:
        return None
    return dict(re.findall(r'^      ([a-z-]+): (read|write)$', match.group(1), re.M))


def validator_script():
    match = re.search(r"python3 - <<'PY'\n(.*?)\n          PY\n", NOTIFY, re.S)
    return '\n'.join(line[10:] for line in match.group(1).split('\n')) + '\n'


def condition_of_notify():
    return re.search(r'^    if: (.*)$', NOTIFY, re.M).group(1)


def evaluate(condition, context):
    """The notify job's `if`: a conjunction of `lhs == literal` terms over a closed set of names."""
    for term in condition.split(' && '):
        negated = re.fullmatch(r"!startsWith\(([A-Za-z0-9_.]+), '([^']*)'\)", term)
        if negated:
            if str(context[negated.group(1)]).startswith(negated.group(2)):
                return False
            continue
        left, right = term.split(' == ')
        want = right[1:-1] if right.startswith("'") else {'true': True, 'false': False}[right]
        if context[left] != want:
            return False
    return True


def published_event(release_id=4242, tag='v0.43.0', **changes):
    event = {'action': 'published',
             'repository': {'id': REPOSITORY_ID, 'full_name': FULL_NAME},
             'release': {'id': release_id, 'tag_name': tag, 'draft': False, 'prerelease': True,
                         'published_at': '2026-11-05T12:00:00Z'}}
    for key, value in changes.items():
        if key in event['release']:
            event['release'][key] = value
        else:
            event[key] = value
    return event


class Receiver(object):
    """The rdy-web workflow-dispatch endpoint. It records each accepted payload. It can fail first."""

    def __init__(self, failures=0):
        self.calls = []
        self.failures = failures

    def dispatch(self, payload):
        if self.failures:
            self.failures -= 1
            raise RuntimeError('receiver unavailable')
        self.calls.append(payload)


class Notify(object):
    """One notify run: the job condition, the real validation script, then the dispatch to the receiver."""

    def __init__(self, test):
        self.tmp = tempfile.TemporaryDirectory()
        test.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.runs = 0

    def run(self, event, receiver, enabled='true', repository_id=str(REPOSITORY_ID), commit=S):
        self.runs += 1
        context = {'github.repository_id': repository_id,
                   'github.event.release.draft': event['release']['draft'],
                   'github.event.release.tag_name': event['release']['tag_name'],
                   'vars.RDY_WEB_DISPATCH_ENABLED': enabled}
        if not evaluate(condition_of_notify(), context):
            return 'skipped'
        work = self.root / ('run-' + str(self.runs))
        work.mkdir()
        (work / 'event.json').write_text(json.dumps(event))
        env = {'PATH': os.environ['PATH'], 'GITHUB_EVENT_PATH': str(work / 'event.json'), 'RUNNER_TEMP': str(work),
               'EXPECTED_SOURCE_COMMIT': commit}
        done = subprocess.run([sys.executable, '-I', '-B', '-c', validator_script()], env=env, capture_output=True)
        if done.returncode != 0:
            return 'refused'
        receiver.dispatch(json.loads((work / 'website-dispatch.json').read_text()))
        return 'dispatched'


def exact_payload(release_id=4242, tag='v0.43.0', commit=S):
    return {'ref': 'main', 'inputs': {'release_id': str(release_id), 'tag_name': tag, 'source_commit': commit}}


class Delivery(unittest.TestCase):
    def test_rsig8_published_event_dispatches_exact_release_identity(self):
        notify, receiver = Notify(self), Receiver()
        self.assertEqual(notify.run(published_event(), receiver), 'dispatched')
        self.assertEqual(receiver.calls, [exact_payload()])
        self.assertEqual(sorted(receiver.calls[0]), ['inputs', 'ref'])
        self.assertEqual(sorted(receiver.calls[0]['inputs']), ['release_id', 'source_commit', 'tag_name'])
        notify.run(published_event(release_id=77, tag='v0.9.0'), receiver, commit='a' * 40)
        self.assertEqual(receiver.calls[1], exact_payload(77, 'v0.9.0', 'a' * 40))
        # The trigger is the real published event and nothing else: no manual, called or synthetic entry.
        self.assertEqual(re.search(r'^on:\n((?:  .*\n)+)', NOTIFY, re.M).group(1),
                         '  release:\n    types: [published]\n')
        for word in ('workflow_dispatch', 'workflow_call', 'repository_dispatch', 'schedule:', 'inputs.',
                     'secrets: inherit'):
            self.assertNotIn(word, NOTIFY)
        for name in all_workflows():
            if name != NOTIFY_PATH.name:
                for word in ('core-release-notify', 'rdy-web', 'RDY_WEB_DISPATCH'):
                    self.assertNotIn(word, text_of(name), name)
        # The unchanged receiver: its path, API version, App names, owner, repository and permission.
        self.assertIn('repos/rdyhq/rdy-web/actions/workflows/core-release-update.yml/dispatches', NOTIFY)
        self.assertIn("-H 'X-GitHub-Api-Version: 2026-03-10'", NOTIFY)
        self.assertIn('--input "$RUNNER_TEMP/website-dispatch.json"', NOTIFY)
        token = re.search(r'uses: actions/create-github-app-token@' + SHA40 + r'.*\n        with:\n((?:          .*\n)+)',
                          NOTIFY)
        self.assertEqual(dict(re.findall(r'^\s+([a-z-]+): (.*)$', token.group(1), re.M)),
                         {'app-id': '${{ vars.RDY_WEB_DISPATCH_APP_ID }}',
                          'private-key': '${{ secrets.RDY_WEB_DISPATCH_APP_PRIVATE_KEY }}',
                          'owner': 'rdyhq', 'repositories': 'rdy-web', 'permission-actions': 'write'})
        # The comment names the publisher App, not a person's token. The publisher App is never used here.
        self.assertNotIn("person's token", NOTIFY)
        self.assertIn("release publisher App's token", NOTIFY)
        self.assertNotIn('RDY_RELEASE_PUBLISHER', code_of(NOTIFY))
        self.assertEqual(re.findall(r'secrets\.[A-Z_]+', NOTIFY), ['secrets.RDY_WEB_DISPATCH_APP_PRIVATE_KEY'])

    def test_rsig8_disabled_or_draft_event_never_dispatches(self):
        notify, receiver = Notify(self), Receiver()
        self.assertEqual(notify.run(published_event(), receiver, enabled=''), 'skipped')
        self.assertEqual(notify.run(published_event(), receiver, enabled='false'), 'skipped')
        self.assertEqual(notify.run(published_event(), receiver, enabled='TRUE'), 'skipped')
        self.assertEqual(notify.run(published_event(draft=True), receiver), 'skipped')
        self.assertEqual(notify.run(published_event(), receiver, repository_id='1'), 'skipped')
        # A docs bundle prerelease is skipped, never failed. Any other non-semver tag still fails in the script.
        self.assertEqual(notify.run(published_event(tag='docs-v0.51.0'), receiver), 'skipped')
        self.assertEqual(notify.run(published_event(tag='v0.51.0'), receiver), 'dispatched')
        receiver.calls.clear()
        # An event that passes the job condition but is not a genuine published event stops in the script.
        for changed in (dict(action='created'), dict(action='edited'), dict(published_at=None), dict(published_at=''),
                        dict(release_id='4242'), dict(release_id=0), dict(release_id=True),
                        dict(tag='v0.43'), dict(tag='0.43.0'), dict(tag='v0.43.0-rc1'), dict(tag='v01.2.3'),
                        dict(tag='candidate/v0.43.0'),
                        dict(repository={'id': 1, 'full_name': FULL_NAME}),
                        dict(repository={'id': REPOSITORY_ID, 'full_name': 'someone/rdy-core'})):
            with self.subTest(changed=changed):
                self.assertEqual(notify.run(published_event(**changed), receiver), 'refused')
        self.assertEqual(notify.run(published_event(), receiver, commit='abc'), 'refused')
        self.assertEqual(notify.run(published_event(), receiver, commit='A' * 40), 'refused')
        self.assertEqual(receiver.calls, [])
        # The activation switch is the variable, read once at the job.
        self.assertEqual(condition_of_notify().count('vars.RDY_WEB_DISPATCH_ENABLED'), 1)
        self.assertEqual(code_of(NOTIFY).count('RDY_WEB_DISPATCH_ENABLED'), 1)
        self.assertIn("github.repository_id == '1403751388'", condition_of_notify())
        self.assertIn('github.event.release.draft == false', condition_of_notify())
        self.assertLess(NOTIFY.index('Validate event'), NOTIFY.index('Create repository-scoped dispatch token'))
        self.assertLess(NOTIFY.index('Create repository-scoped dispatch token'), NOTIFY.index('Dispatch private website'))

    def test_rsig8_dispatch_retry_does_not_republish(self):
        # A failed notification is a distinct state. A retry re-runs the same event and sends the same payload.
        notify, receiver = Notify(self), Receiver(failures=1)
        event = published_event()
        with self.assertRaises(RuntimeError):
            notify.run(event, receiver)
        self.assertEqual(receiver.calls, [])
        self.assertEqual(notify.run(event, receiver), 'dispatched')
        self.assertEqual(receiver.calls, [exact_payload()])
        # The workflow can only notify: no checkout, release call, tag, upload, build or signing.
        code = code_of(NOTIFY)
        for word in ('actions/checkout', 'gh release', 'releases', 'git ', 'upload', 'cargo', 'sign', 'download',
                     'git/refs', 'contents:'):
            self.assertNotIn(word, code)
        self.assertEqual(code.count('gh api'), 1)
        self.assertEqual(code.count('--method POST'), 1)
        # Concurrency is per release id, and a queued retry is never cancelled.
        self.assertIn('group: website-notify-${{ github.event.release.id }}\n  cancel-in-progress: false', NOTIFY)
        # The release path never dispatches the website, so nothing else can stand in for the event.
        publish_source = (ROOT / 'scripts/release/publish.py').read_text()
        for word in ('rdy-web', 'core-release-update', 'core-release-notify'):
            self.assertNotIn(word, publish_source)
        self.assertNotIn('core-release-notify', pub.WORKFLOW.split('jobs:', 1)[1])


class Boundaries(unittest.TestCase):
    def stage(self):
        stage = pub.Stage()
        self.addCleanup(stage.close)
        return stage

    def test_rsig8_signing_and_publishing_permissions_are_separate(self):
        for name in RELEASE_FILES:
            self.assertRegex(text_of(name), r'(?m)^permissions: \{\}$', name)
        for name in all_workflows():
            self.assertNotIn('secrets: inherit', text_of(name), name)
        # OIDC: the caller's one reusable-call job, and the callee's claims and sign jobs. No other job anywhere.
        holders = {}
        for name in all_workflows():
            for job, block in jobs_of(text_of(name)).items():
                if (job_permissions(block) or {}).get('id-token') == 'write':
                    holders.setdefault(name, []).append(job)
        self.assertEqual(holders, {'release.yml': ['sign'], 'release-sign.yml': ['claims', 'sign']})
        caller = jobs_of(text_of('release.yml'))
        callee = jobs_of(text_of('release-sign.yml'))
        self.assertIn('    uses: adrenickchrome/rdy-core/.github/workflows/release-sign.yml@', caller['sign'])
        self.assertNotIn('secrets:', caller['sign'])
        self.assertNotIn('environment:', caller['sign'])
        # Propagation: the caller's call job grants at least what every callee job asks.
        granted = job_permissions(caller['sign'])
        self.assertEqual(granted, {'actions': 'read', 'contents': 'read', 'id-token': 'write', 'security-events': 'read'})
        for job, block in callee.items():
            for key, level in (job_permissions(block) or {}).items():
                with self.subTest(job=job, key=key):
                    self.assertGreaterEqual(LEVEL.get(granted.get(key), 0), LEVEL[level])
        # The signing side has no publisher secret, no App token and no write to the repository.
        signer_text = text_of('release-sign.yml')
        for word in ('secrets.', 'create-github-app-token', 'RDY_RELEASE_PUBLISHER', 'RDY_PUBLISHER_TOKEN',
                     'gh release', 'contents: write', 'concurrency'):
            self.assertNotIn(word, signer_text)
        # The publisher App lives in the caller's promote job only. The web App lives in the notify workflow only.
        for name in all_workflows():
            text = text_of(name)
            if name != 'release.yml':
                self.assertNotIn('RDY_RELEASE_PUBLISHER', text, name)
                self.assertNotIn('RDY_PUBLISHER_TOKEN', text, name)
            if name != NOTIFY_PATH.name:
                self.assertNotIn('RDY_WEB_DISPATCH', text, name)
        for job, block in caller.items():
            self.assertEqual('RDY_RELEASE_PUBLISHER' in block, job == 'promote', job)
        self.assertEqual(text_of('release.yml').count('create-github-app-token'), 1)
        self.assertEqual(NOTIFY.count('create-github-app-token'), 1)
        # The notify job holds no repository permission, no environment and no OIDC.
        notify_jobs = jobs_of(NOTIFY)
        self.assertEqual(list(notify_jobs), ['notify'])
        self.assertIsNone(job_permissions(notify_jobs['notify']))
        self.assertNotIn('environment:', NOTIFY)
        # Repository writes: exactly the reserve, draft and final-tag jobs of the caller, and none has OIDC or an App.
        writers = sorted(j for j, b in caller.items() if (job_permissions(b) or {}).get('contents') == 'write')
        self.assertEqual(writers, ['draft', 'final-tag', 'reserve'])
        for job in writers:
            self.assertNotIn('id-token', job_permissions(caller[job]))
            self.assertNotIn('create-github-app-token', caller[job])
        self.assertEqual(job_permissions(caller['promote']), {'actions': 'read', 'contents': 'read'})
        # The candidate workflow has no secret, no OIDC, no environment and no write.
        candidate = code_of(text_of('release-candidate.yml'))
        for word in ('secrets.', 'id-token', 'environment:'):
            self.assertNotIn(word, candidate)
        self.assertNotRegex(candidate, r': write\b')
        # The lane: one fixed group for the caller.
        self.assertIn('group: rdy-core-release-authority\n  cancel-in-progress: false', text_of('release.yml'))
        for name in RELEASE_FILES:
            self.assertNotIn('pull_request', text_of(name), name)

    def test_rsig8_workflow_actions_and_signer_revision_are_pinned(self):
        for name in all_workflows():
            for line in text_of(name).splitlines():
                match = re.match(r'\s*(?:- )?uses: (\S+)', line)
                if match is None:
                    continue
                with self.subTest(workflow=name, uses=match.group(1)):
                    self.assertRegex(match.group(1),
                                     r'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@' + SHA40 + '$')
        for name in RELEASE_FILES:
            for line in text_of(name).splitlines():
                if re.match(r'\s*(?:- )?uses: actions/', line):
                    self.assertRegex(line, r'@' + SHA40 + r' # v\d')
        # The signer revision: one literal in every place that names it, never an expression or a movable ref.
        caller = text_of('release.yml')
        places = (re.findall(r'uses: adrenickchrome/rdy-core/\.github/workflows/release-sign\.yml@(\S+)', caller)
                  + re.findall(r'^\s+signer_sha: (\S+)$', caller, re.M)
                  + re.findall(r'--signer-sha (\S+)', caller)
                  + re.findall(r'repository: adrenickchrome/rdy-core\n\s+ref: (\S+)', caller))
        self.assertGreaterEqual(len(places), 10)
        self.assertEqual(len(set(places)), 1, places)
        self.assertRegex(places[0], '^' + SHA40 + '$')
        for movable in ('@main', '@master', '@HEAD', 'refs/heads', 'ref: main'):
            self.assertNotIn(movable, caller)
        # The callee binds its input to the literal revision that GitHub says this run referenced.
        signer = text_of('release-sign.yml')
        self.assertIn("found[0].get('sha') != sha or found[0]['path'] != prefix + sha", signer)
        self.assertIn('ref: ${{ needs.revision.outputs.sha }}', signer)
        # The caller is a manual dispatch only.
        self.assertRegex(caller, r'(?m)^on:\n  workflow_dispatch:\n')
        self.assertNotIn('\n  push:', caller)
        self.assertNotIn('schedule:', caller)
        # A real linter, when the machine has one.
        actionlint = shutil.which('actionlint')
        if actionlint:
            done = subprocess.run([actionlint] + [str(WORKFLOWS / n) for n in all_workflows()],
                                  capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    def test_rsig8_candidate_never_exports_test_trust(self):
        code = code_of(text_of('release-candidate.yml'))
        for word in ('rehearse', 'allowed_signers', 'SHA256SUMS.sig', 'ssh-keygen', 'rdy-release', 'trust'):
            self.assertNotIn(word, code, word)
        self.assertNotIn('rehearse', text_of('release.yml'))
        self.assertNotIn('rehearse', text_of('release-sign.yml'))
        build = (ROOT / 'scripts/release/candidate.py').read_text()
        for word in ('allowed_signers', 'SHA256SUMS.sig', 'ssh-keygen', 'rehears'):
            self.assertNotIn(word, build, word)
        # The rehearsal boundary keeps its root and signature inside its own folders.
        rehearse = (ROOT / 'scripts/release/rehearse.py').read_text()
        self.assertIn("boundary / 'trust' / 'allowed_signers'", rehearse)
        self.assertIn("download / 'SHA256SUMS.sig'", rehearse)
        # The sealed candidate zip of the fake release holds the two archives, their sums and the record. Nothing else.
        stage = self.stage()
        with zipfile.ZipFile(stage.git.paths[pub.base.CAND_ART]) as z:
            names = sorted(z.namelist())
        archives = sorted(pub.archive_files(stage.w))
        self.assertEqual(len(archives), 2)
        self.assertEqual(names, sorted(archives + ['SHA256SUMS', 'candidate.json']))
        # No release asset, draft or public, is a trust root, and the asset set is the closed one.
        stage.ready()
        stage.final_tag()
        stage.promote()
        for row in stage.releases.rows:
            for asset in row['assets']:
                self.assertNotRegex(asset['name'], r'(?i)allowed|trust|\.pub$|\.pem$|\.key$')
        self.assertEqual(sorted(a['name'] for a in stage.releases.rows[0]['assets']), sorted(pub.stage_names(stage)))


class Sequence(unittest.TestCase):
    def stage(self):
        stage = pub.Stage()
        self.addCleanup(stage.close)
        return stage

    def signed_after_crash(self, stage):
        """Sign once, lose the retention acknowledgement, and recover the retained bytes on restart."""
        rig = stage.rig
        rig.reserve_version()
        ctrl = rig.session()
        rig.table.fail_next('SIGNATURE', 'lost_ack')
        with self.assertRaises(pub.CONTROLLER.Refused):
            rig.sign(ctrl)
        fresh = rig.session(rig.rerun())
        recovered = fresh.recover(rig.tuple_bytes())
        self.assertEqual(rig.kms.signs, 1)
        stage.authorization = fresh.authorization()
        stage.transfer_signature(recovered.signature, stage.authorization)
        return recovered.signature

    def test_rsig8_full_mock_release_and_conflicting_retry(self):
        stage = self.stage()
        rig, releases = stage.rig, stage.releases
        # Candidate to signature: one Sign call, a crash on retention, recovery of the retained bytes.
        signature = self.signed_after_crash(stage)
        # A conflicting restart cannot sign again.
        for attempt in (3, 4):
            again = rig.session(rig.rerun(attempt=attempt))
            with self.assertRaises(pub.CONTROLLER.Refused):
                rig.sign(again)
        self.assertEqual(rig.kms.signs, 1)
        # Draft: a lost creation acknowledgement and a lost upload acknowledgement each read back once.
        releases.faults['create_draft'] = (True, 1)
        releases.faults['upload SHA256SUMS.sig'] = (True, 1)
        stage.draft()
        self.assertEqual(releases.mutations.count('create_draft'), 1)
        self.assertEqual(releases.mutations.count('upload SHA256SUMS.sig'), 1)
        self.assertEqual(len(releases.rows), 1)
        self.assertEqual(sorted(a['name'] for a in releases.draft_row()['assets']), sorted(pub.stage_names(stage)))
        draft_id = releases.draft_row()['id']
        # Both native genuine checks, then the final tag. A second tag job in the same state records a retry, with no write.
        stage.both_genuine()
        stage.final_tag()
        self.assertEqual(json.loads(stage.final_tag())['result'], 'recorded_retry')
        self.assertEqual(releases.mutations.count('create_final_tag'), 1)
        # Promotion: a lost acknowledgement reads back and never publishes twice.
        releases.faults['publish_draft'] = (True, 1)
        self.assertTrue(json.loads(stage.promote()))
        self.assertEqual(releases.mutations.count('publish_draft'), 1)
        public = releases.public_release('v0.43.0')
        self.assertEqual(public.release_id, draft_id)
        self.assertFalse(public.draft)
        # A conflicting retry after publication: a second promote, draft, final tag or Sign changes nothing.
        published = list(releases.mutations)
        for call in (stage.promote, stage.draft, stage.final_tag):
            try:
                call()
            except pub.ControllerError:
                pass
        with self.assertRaises(pub.CONTROLLER.Refused) as caught:
            rig.sign(rig.session(rig.rerun(attempt=5)))
        self.assertEqual(str(caught.exception), 'reservation conflict: v0.43.0 is published; this observation is terminal')
        self.assertEqual(releases.mutations, published)
        self.assertEqual(releases.mutations.count('publish_draft'), 1)
        self.assertEqual(releases.mutations.count('create_draft'), 1)
        self.assertEqual(releases.mutations.count('create_final_tag'), 1)
        self.assertEqual(len(releases.rows), 1)
        self.assertEqual(rig.kms.signs, 1)
        self.assertEqual(stage.w.members[(pub.SIG_ART, 'SHA256SUMS.sig')], signature)
        # The notification: the real published event of that release id, one failed delivery, one retry.
        row = releases.rows[0]
        event = published_event(release_id=row['id'], tag=row['tag'])
        notify, receiver = Notify(self), Receiver(failures=1)
        with self.assertRaises(RuntimeError):
            notify.run(event, receiver)
        self.assertEqual(notify.run(event, receiver), 'dispatched')
        self.assertEqual(receiver.calls, [exact_payload(row['id'], 'v0.43.0', S)])
        self.assertEqual(releases.mutations, published, 'the notification never touches the release')
        self.assertEqual(rig.kms.signs, 1)

    def test_rsig8_design_tag_absent_or_failed_never_gates_publication(self):
        # The sequence runs with no design tag anywhere, publishes, and notifies.
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        stage.promote()
        releases = stage.releases
        self.assertEqual(releases.mutations.count('publish_draft'), 1)
        self.assertFalse(releases.public_release('v0.43.0').draft)
        self.assertEqual([r for r in stage.w.refs if 'design' in r], [])
        # The later hand-off to the design owner fails once. Publication stays successful and nothing retries it.
        calls = []

        def hand_off(version):
            calls.append(version)
            raise RuntimeError('the design tag could not be created')

        before = list(releases.mutations)
        signs = stage.rig.kms.signs
        with self.assertRaises(RuntimeError):
            hand_off('0.43.0')
        self.assertEqual(calls, ['0.43.0'])
        self.assertEqual(releases.mutations, before)
        self.assertEqual(stage.rig.kms.signs, signs)
        self.assertFalse(releases.public_release('v0.43.0').draft)
        row = releases.rows[0]
        receiver = Receiver()
        self.assertEqual(Notify(self).run(published_event(release_id=row['id'], tag=row['tag']), receiver), 'dispatched')
        self.assertEqual(len(receiver.calls), 1)
        self.assertEqual(calls, ['0.43.0'], 'no retry of the hand-off from the release path')
        # Source controls: no workflow or release script waits for, creates or reads a design tag.
        scripts = ''.join(p.read_text() for p in (ROOT / 'scripts/release').glob('*.py'))
        workflows = ''.join(text_of(n) for n in all_workflows())
        for text in (scripts, workflows):
            self.assertNotRegex(text, r'(?i)design[ -]tag')
            self.assertNotRegex(text, r'(?i)rdy-design')
        self.assertNotIn('design', text_of('release.yml').lower())
        self.assertNotIn('design', NOTIFY.lower())


if __name__ == '__main__':
    unittest.main()
