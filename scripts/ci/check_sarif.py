#!/usr/bin/env python3
"""Fail on high/critical CodeQL security results and analyzer errors, not upload success."""
import json
import math
from pathlib import Path
import re
import subprocess
import sys


MAX_DIAGNOSTICS = 100
ROOT = Path(__file__).resolve().parents[2]


def blocking_diagnostic(result, rule, score, run, repository_paths):
    """Only emit identifiers and tracked relative locations, never SARIF prose/URIs."""
    identifier = rule['id']
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_./-]{0,159}', identifier):
        identifier = '[withheld-invalid-identifier]'
    diagnostic = {'rule_id': identifier,
                  'severity': score if score is not None else 'unscored-error',
                  'location': None}
    locations = result.get('locations', [])
    if not isinstance(locations, list):
        return diagnostic
    for location in locations[:10]:
        if not isinstance(location, dict):
            continue
        physical = location.get('physicalLocation', {})
        if not isinstance(physical, dict):
            continue
        artifact = physical.get('artifactLocation', {})
        region = physical.get('region', {})
        if not isinstance(artifact, dict) or not isinstance(region, dict):
            continue
        # Some SARIF writers reference run.artifacts instead of repeating the URI.
        if 'index' in artifact:
            index = artifact['index']
            inventory = run.get('artifacts', [])
            if type(index) is not int or not isinstance(inventory, list) or not 0 <= index < len(inventory):
                continue
            entry = inventory[index]
            indexed = entry.get('location', {}) if isinstance(entry, dict) else {}
            if not isinstance(indexed, dict):
                continue
            if any(key in artifact and key in indexed and artifact[key] != indexed[key]
                   for key in ('uri', 'uriBaseId')):
                continue
            artifact = dict(indexed, **artifact)
        uri = artifact.get('uri')
        line = region.get('startLine')
        if (not isinstance(uri, str) or len(uri) > 1024
                or not re.fullmatch(r'[A-Za-z0-9_. /-]+', uri)
                or any(part in ('', '.', '..') for part in uri.split('/'))
                or artifact.get('uriBaseId') not in (None, '%SRCROOT%')
                or uri not in repository_paths
                or type(line) is not int or not 1 <= line <= 2147483647):
            continue
        diagnostic['location'] = {'path': uri, 'line': line}
        break
    return diagnostic


def reference_index(value):
    if type(value) is not int or value < -1:
        raise ValueError('invalid reference index')
    return value


def select_reference(reference, values, fields):
    """Resolve one component/descriptor without flattening distinct rule namespaces."""
    if not isinstance(reference, dict) or set(reference) - {'index', 'properties', *fields}:
        raise ValueError('invalid reference')
    candidates = list(values)
    if 'index' in reference:
        index = reference_index(reference['index'])
        if index >= 0:
            if index >= len(values):
                raise ValueError('reference index outside inventory')
            candidates = [values[index]]
    for field in fields:
        if field in reference:
            value = reference[field]
            if not isinstance(value, str) or not value:
                raise ValueError('invalid reference identity')
            candidates = [item for item in candidates if item.get(field) == value]
    if len(candidates) != 1:
        raise ValueError('unknown or ambiguous reference')
    return candidates[0]


def result_rule(result, driver, extensions):
    if not isinstance(result, dict):
        raise ValueError('invalid result')
    reference = result.get('rule', {})
    if not isinstance(reference, dict) or set(reference) - {'id', 'index', 'guid', 'toolComponent', 'properties'}:
        raise ValueError('invalid rule reference')
    component_ref = reference.get('toolComponent', {})
    if not isinstance(component_ref, dict) or set(component_ref) - {'name', 'index', 'guid', 'properties'}:
        raise ValueError('invalid component reference')
    if 'index' in component_ref and reference_index(component_ref['index']) >= 0:
        component = select_reference(component_ref, extensions, ('name', 'guid'))
    elif any(key in component_ref for key in ('name', 'guid')):
        component = select_reference(component_ref, [driver] + extensions, ('name', 'guid'))
    else:
        component = driver
    # A component index refers to tool.extensions; -1/omitted leaves identity lookup
    # or the driver default. Rule index refers to this component's own rules.
    rule_ref = {key: reference[key] for key in ('id', 'index', 'guid') if key in reference}
    for outer, inner in (('ruleId', 'id'), ('ruleIndex', 'index')):
        if outer in result:
            if inner in rule_ref and rule_ref[inner] != result[outer]:
                raise ValueError('inconsistent rule reference')
            rule_ref[inner] = result[outer]
    if not rule_ref or (set(rule_ref) == {'index'} and reference_index(rule_ref['index']) == -1):
        raise ValueError('missing rule identity')
    return select_reference(rule_ref, component.get('rules', []), ('id', 'guid'))


def inspect_sarif(document, repository_paths=None):
    if not isinstance(document, dict) or document.get('version') != '2.1.0':
        raise ValueError('unsupported SARIF')
    runs = document.get('runs')
    if not isinstance(runs, list) or not runs:
        raise ValueError('missing analysis runs')
    count = blocked = 0
    diagnostics = []
    for run in runs:
        driver = run['tool']['driver']
        if driver.get('name') != 'CodeQL':
            raise ValueError('unexpected analyzer')
        extensions = run['tool'].get('extensions', [])
        if not isinstance(extensions, list):
            raise ValueError('invalid extensions')
        rule_count = 0
        for component in [driver] + extensions:
            if not isinstance(component, dict) or not isinstance(component.get('name'), str):
                raise ValueError('invalid tool component')
            rules = component.get('rules', [])
            if not isinstance(rules, list):
                raise ValueError('invalid rules')
            ids = []
            for rule in rules:
                if not isinstance(rule, dict) or not isinstance(rule.get('id'), str) or not rule['id']:
                    raise ValueError('invalid rule identity')
                ids.append(rule['id'])
            if len(set(ids)) != len(ids):
                raise ValueError('duplicate component rule IDs')
            rule_count += len(rules)
        if not rule_count:
            raise ValueError('empty rule inventory')
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
            # Suppressed results remain visible and are evaluated. No implicit waiver.
            rule = result_rule(result, driver, extensions)
            severity = rule.get('properties', {}).get('security-severity')
            level = result.get('level', rule.get('defaultConfiguration', {}).get('level', 'warning'))
            if severity is not None:
                if isinstance(severity, bool):
                    raise ValueError('invalid security severity')
                score = float(severity)
                if not math.isfinite(score) or not 0 <= score <= 10:
                    raise ValueError('invalid security severity')
                blocking = score >= 7
            else:
                score = None
                blocking = level == 'error'
            blocked += int(blocking)
            if blocking and repository_paths is not None and len(diagnostics) < MAX_DIAGNOSTICS:
                diagnostics.append(blocking_diagnostic(result, rule, score, run, repository_paths))
            count += 1
    summary = {'results': count, 'blocking_results': blocked}
    if repository_paths is not None:
        summary['blocking_diagnostics'] = diagnostics
    return summary


def main():
    paths = sorted(Path(sys.argv[1]).rglob('*.sarif'))
    if not paths:
        raise ValueError('no SARIF output')
    repository_paths = set(subprocess.check_output(
        ['git', 'ls-files', '-z'], cwd=ROOT).decode('utf-8').split('\0')) - {''}
    summaries = [inspect_sarif(json.loads(p.read_text()), repository_paths) for p in paths]
    diagnostics = [item for summary in summaries for item in summary['blocking_diagnostics']][:MAX_DIAGNOSTICS]
    blocked = sum(s['blocking_results'] for s in summaries)
    print(json.dumps({'files': len(paths), 'results': sum(s['results'] for s in summaries),
                      'blocking_results': blocked, 'blocking_diagnostics': diagnostics,
                      'diagnostics_omitted': blocked - len(diagnostics)}))
    return int(any(s['blocking_results'] for s in summaries))


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError, IndexError, subprocess.CalledProcessError):
        raise SystemExit('CodeQL result policy refused incomplete or invalid analysis')
