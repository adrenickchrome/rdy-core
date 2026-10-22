#!/usr/bin/env python3
"""Scan tracked source; report only locations, never matched values."""
import bisect
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def source_input(root):
    names = subprocess.check_output(['git', '-c', 'core.fsmonitor=false', '-c',
                                     'core.hooksPath=/dev/null', 'ls-files', '-z'], cwd=root).split(b'\0')
    chunks, files = [], []
    next_line = 1
    for raw in filter(None, names):
        name = os.fsdecode(raw)
        path = root / name
        if path.is_symlink():
            data = os.fsencode(os.readlink(path))
        elif path.is_file():
            data = path.read_bytes()
        else:
            raise ValueError('tracked source is unavailable')
        # Record exact stream line boundaries. Do not read outside tracked symlinks.
        lines = data.split(b'\n')
        files.append({'path': name, 'start': next_line + 2, 'lines': lines})
        chunk = b'\nFILE ' + raw + b'\n' + data + b'\n'
        next_line += chunk.count(b'\n')
        chunks.append(chunk)
    if not files:
        raise ValueError('empty source inventory')
    return b''.join(chunks), files


def fixture_keys(document):
    if not isinstance(document, dict) or document.get('schema_version') != 1:
        raise ValueError('invalid fixture dispositions')
    rows = document.get('fixtures')
    if not isinstance(rows, list):
        raise ValueError('invalid fixture list')
    keys = set()
    for row in rows:
        if not isinstance(row, dict) or set(row) != {'path', 'line', 'rule', 'line_sha256', 'reason'}:
            raise ValueError('invalid fixture entry')
        path = row['path']
        if not isinstance(path, str) or PurePosixPath(path).is_absolute() or '..' in PurePosixPath(path).parts:
            raise ValueError('unsafe fixture path')
        if type(row['line']) is not int or row['line'] < 1 or row['rule'] != 'generic-api-key':
            raise ValueError('invalid fixture scope')
        if not isinstance(row['line_sha256'], str) or not re.fullmatch('[0-9a-f]{64}', row['line_sha256']):
            raise ValueError('invalid fixture digest')
        if not isinstance(row['reason'], str) or not row['reason'].strip():
            raise ValueError('missing fixture explanation')
        key = (path, row['line'], row['rule'], row['line_sha256'])
        if key in keys:
            raise ValueError('duplicate fixture disposition')
        keys.add(key)
    return keys


def summarize(findings, files, dispositions):
    if not isinstance(findings, list):
        raise ValueError('invalid scanner findings')
    keys = fixture_keys(dispositions)
    starts = [f['start'] for f in files]
    remaining, fixtures = [], []
    for finding in findings:
        if not isinstance(finding, dict) or not isinstance(finding.get('RuleID'), str):
            raise ValueError('invalid scanner result')
        first, last = finding.get('StartLine'), finding.get('EndLine')
        if type(first) is not int or type(last) is not int or first < 1 or last < first:
            raise ValueError('invalid scanner location')
        index = bisect.bisect_right(starts, first) - 1
        if index < 0 or last >= starts[index] + len(files[index]['lines']):
            raise ValueError('scanner location outside source')
        file = files[index]
        line = first - file['start'] + 1
        location = {'path': file['path'], 'line': line, 'end_line': last - file['start'] + 1,
                    'rule': finding['RuleID']}
        digest = hashlib.sha256(file['lines'][line - 1]).hexdigest()
        key = (file['path'], line, finding['RuleID'], digest)
        # Only one exact immutable source line is eligible. Multiline/new/changed
        # matches are never covered by a known-fixture disposition.
        (fixtures if first == last and key in keys else remaining).append(location)
    return {'tracked_files': len(files), 'findings': len(findings),
            'reviewed_fixture_matches': len(fixtures), 'blocking_findings': len(remaining),
            'locations': remaining, 'fixture_locations': fixtures}


def scan(tool, root=ROOT):
    data, files = source_input(root)
    with tempfile.TemporaryDirectory(prefix='rdy-secret-scan-') as scratch:
        report = Path(scratch) / 'report.json'
        env = {k: v for k, v in os.environ.items() if not k.startswith('GITLEAKS_')}
        result = subprocess.run([str(tool), 'stdin', '--config', str(ROOT / 'scripts/ci/gitleaks.toml'),
                                 '--ignore-gitleaks-allow', '--redact=100', '--no-banner', '--no-color',
                                 '--report-format', 'json', '--report-path', str(report)],
                                input=data, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, env=env)
        if result.returncode not in (0, 1) or not report.is_file():
            raise ValueError('scanner did not complete')
        findings = json.loads(report.read_text())
        if not isinstance(findings, list) or bool(findings) != (result.returncode == 1):
            raise ValueError('inconsistent scanner result')
        dispositions = json.loads((ROOT / 'scripts/ci/secret-fixtures.json').read_text())
        summary = summarize(findings, files, dispositions)
        print(json.dumps(summary))
        return int(bool(summary['blocking_findings']))


if __name__ == '__main__':
    try:
        sys.exit(scan(Path(sys.argv[1]).resolve()))
    except (ValueError, KeyError, TypeError, OSError, IndexError):
        raise SystemExit('secret scan refused incomplete source or scanner output')
