#!/usr/bin/env python3
"""Fail on high/critical CodeQL security results and analyzer errors, not upload success."""
import json
import math
from pathlib import Path
import sys


def inspect_sarif(document):
    if not isinstance(document, dict) or document.get('version') != '2.1.0':
        raise ValueError('unsupported SARIF')
    runs = document.get('runs')
    if not isinstance(runs, list) or not runs:
        raise ValueError('missing analysis runs')
    count = blocked = 0
    for run in runs:
        driver = run['tool']['driver']
        if driver.get('name') != 'CodeQL':
            raise ValueError('unexpected analyzer')
        rules = driver.get('rules', [])
        if not isinstance(rules, list) or not rules:
            raise ValueError('invalid rules')
        by_id = {r['id']: r for r in rules}
        if len(by_id) != len(rules):
            raise ValueError('duplicate rule IDs')
        for invocation in run.get('invocations', []):
            if invocation.get('executionSuccessful') is False:
                raise ValueError('analyzer execution failed')
            notifications = (invocation.get('toolExecutionNotifications', [])
                             + invocation.get('toolConfigurationNotifications', []))
            for notification in notifications:
                if notification.get('level') == 'error':
                    raise ValueError('analysis reported an error')
        results = run.get('results')
        if not isinstance(results, list):
            raise ValueError('missing results')
        for result in results:
            if not isinstance(result, dict) or result.get('ruleId') not in by_id:
                raise ValueError('unknown result rule')
            # Suppressed results remain visible and are evaluated. No implicit waiver.
            rule = by_id[result['ruleId']]
            severity = rule.get('properties', {}).get('security-severity')
            level = result.get('level', rule.get('defaultConfiguration', {}).get('level', 'warning'))
            if severity is not None:
                if isinstance(severity, bool):
                    raise ValueError('invalid security severity')
                score = float(severity)
                if not math.isfinite(score) or not 0 <= score <= 10:
                    raise ValueError('invalid security severity')
                blocked += int(score >= 7)
            else:
                blocked += int(level == 'error')
            count += 1
    return {'results': count, 'blocking_results': blocked}


def main():
    paths = sorted(Path(sys.argv[1]).rglob('*.sarif'))
    if not paths:
        raise ValueError('no SARIF output')
    summaries = [inspect_sarif(json.loads(p.read_text())) for p in paths]
    print(json.dumps({'files': len(paths), 'results': sum(s['results'] for s in summaries),
                      'blocking_results': sum(s['blocking_results'] for s in summaries)}))
    return int(any(s['blocking_results'] for s in summaries))


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError, IndexError):
        raise SystemExit('CodeQL result policy refused incomplete or invalid analysis')
