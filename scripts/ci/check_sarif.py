#!/usr/bin/env python3
"""CodeQL gate in two steps.

sarif DIR: fail on analyzer errors and on unreadable or malformed SARIF; list every high or
critical result (rule id, severity, path:line). Results alone do not fail this step.

alerts: fail on any open high or critical code-scanning alert of one tool on one ref, in any
analysis category. A false positive is checked and then dismissed on its alert with its
reason, so a dismissed or fixed alert does not block. If the alerts cannot be read, this fails.
"""
import argparse
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request


SUMMARY_LINES = 100
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
        invocations = run.get('invocations')
        if not isinstance(invocations, list) or not invocations:
            raise ValueError('missing analyzer invocations')
        for invocation in invocations:
            # The execution status must be stated, as a JSON boolean.
            if not isinstance(invocation, dict) or type(invocation.get('executionSuccessful')) is not bool:
                raise ValueError('invalid analyzer invocation')
            if not invocation['executionSuccessful']:
                raise ValueError('analyzer execution failed')
            notifications = []
            for key in ('toolExecutionNotifications', 'toolConfigurationNotifications'):
                value = invocation.get(key, [])
                if not isinstance(value, list):
                    raise ValueError('invalid analyzer notifications')
                notifications += value
            for notification in notifications:
                if not isinstance(notification, dict):
                    raise ValueError('invalid analyzer notification')
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
            if blocking and repository_paths is not None:
                diagnostics.append(blocking_diagnostic(result, rule, score, run, repository_paths))
            count += 1
    summary = {'results': count, 'blocking_results': blocked}
    if repository_paths is not None:
        summary['blocking_diagnostics'] = diagnostics
    return summary


def severity_label(score):
    if not isinstance(score, float):
        return 'error (no security severity)'
    return ('critical' if score >= 9 else 'high') + ' ' + format(score, 'g')


def result_lines(diagnostics):
    lines = []
    for item in diagnostics:
        location = item['location']
        where = location['path'] + ':' + str(location['line']) if location else '(location withheld)'
        lines.append(item['rule_id'] + '  ' + severity_label(item['severity']) + '  ' + where)
    return lines


def write_summary(title, lines):
    """The step summary shows at most SUMMARY_LINES lines; the log always has the full list."""
    path = os.environ.get('GITHUB_STEP_SUMMARY')
    if not path:
        return
    with open(path, 'a', encoding='utf-8') as summary:
        summary.write('## ' + title + '\n\n')
        shown = lines[:SUMMARY_LINES]
        summary.write(''.join('- `' + line + '`\n' for line in shown) if shown else 'None.\n')
        if len(lines) > len(shown):
            summary.write('\n' + str(len(lines) - len(shown)) + ' more not shown here. The full list is in the '
                          'job log and in the codeql-sarif artifact.\n')
        summary.write('\n')


def sarif_main(directory):
    paths = sorted(Path(directory).rglob('*.sarif'))
    if not paths:
        raise ValueError('no SARIF output')
    repository_paths = set(subprocess.check_output(
        ['git', 'ls-files', '-z'], cwd=ROOT).decode('utf-8').split('\0')) - {''}
    summaries = [inspect_sarif(json.loads(p.read_text()), repository_paths) for p in paths]
    diagnostics = [item for summary in summaries for item in summary['blocking_diagnostics']]
    high = sum(s['blocking_results'] for s in summaries)
    results = sum(s['results'] for s in summaries)
    lines = result_lines(diagnostics)
    print('CodeQL SARIF: ' + str(len(paths)) + ' file(s), ' + str(results) + ' result(s), '
          + str(high) + ' high or critical. Open alerts decide whether they block.')
    for line in lines:
        print(line)
    write_summary('CodeQL high and critical results (' + str(high) + ')', lines)
    return 0


# The code-scanning alert fields this gate reads, with every value the REST API documents.
ALERT_STATES = ('open', 'dismissed', 'fixed')
SEVERITY_LEVELS = ('low', 'medium', 'high', 'critical')
BLOCKING_LEVELS = ('high', 'critical')
PAGE_SIZE = 100
MAX_PAGES = 100


class AlertQueryError(Exception):
    pass


def clean(value, limit=200):
    text = str(value)
    return ''.join(c if c.isprintable() else '?' for c in text)[:limit]


def blocking_alerts(alerts, tool):
    """Open alerts of this tool at security severity high or critical, in any category.

    Every record is checked first: an alert this gate cannot read fails it, never passes it.
    A security severity of null (an unscored rule) is valid and does not block.
    """
    if not isinstance(alerts, list):
        raise ValueError('invalid alert list')
    blocking = []
    for alert in alerts:
        if not isinstance(alert, dict) or type(alert.get('number')) is not int:
            raise ValueError('invalid alert')
        state, rule = alert.get('state'), alert.get('rule')
        tool_info, instance = alert.get('tool'), alert.get('most_recent_instance')
        if not isinstance(rule, dict) or not isinstance(tool_info, dict) or not isinstance(instance, dict):
            raise ValueError('invalid alert')
        if not isinstance(state, str) or state not in ALERT_STATES:
            raise ValueError('unknown alert state')
        name = tool_info.get('name')
        if not isinstance(name, str) or not name:
            raise ValueError('alert without a tool name')
        if 'security_severity_level' not in rule:
            raise ValueError('alert without a security severity field')
        level = rule['security_severity_level']
        if level is not None and (not isinstance(level, str) or level not in SEVERITY_LEVELS):
            raise ValueError('unknown alert security severity')
        if state == 'open' and name == tool and level in BLOCKING_LEVELS:
            blocking.append(alert)
    return blocking


def alert_line(alert):
    rule = alert['rule']
    location = alert['most_recent_instance'].get('location') or {}
    where = clean(location.get('path', '?')) + ':' + clean(location.get('start_line', '?'))
    return ('#' + str(alert['number']) + '  ' + clean(rule.get('id', '?')) + '  '
            + clean(rule.get('security_severity_level')) + '  ' + where)


def has_next(values):
    """Whether any Link field value names a rel="next" page. A malformed value raises.

    Only the presence of the relation is read; the gate never requests a URL the server sent.
    """
    found = False
    for value in values or []:
        if not isinstance(value, str):
            raise AlertQueryError('invalid Link header')
        for part in value.split(','):
            if not part.strip():
                continue
            match = re.fullmatch(r'\s*<[^<>\s]+>((?:\s*;\s*[A-Za-z]+="[^"]*")*)\s*', part)
            if not match:
                raise AlertQueryError('invalid Link header')
            if any('next' in rel.split() for rel in re.findall(r'rel="([^"]*)"', match.group(1))):
                found = True
    return found


def page_url(api, repository, tool, ref, page):
    query = {'tool_name': tool, 'ref': ref, 'state': 'open', 'per_page': PAGE_SIZE, 'page': page}
    return api.rstrip('/') + '/repos/' + repository + '/code-scanning/alerts?' + urllib.parse.urlencode(query)


def fetch_alerts(api, repository, token, tool, ref):
    """All open alerts of the tool on the ref. Any failure raises AlertQueryError.

    Every page URL is built here (page=1, 2, 3, ...). The next page is read while the Link header
    names a next page or the page is full; only a short page with no next page ends the list.
    """
    if not api.startswith('https://'):
        raise AlertQueryError('the API URL is not HTTPS')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository or ''):
        raise AlertQueryError('GITHUB_REPOSITORY is missing or invalid')
    if not token:
        raise AlertQueryError('GITHUB_TOKEN is missing')
    alerts = []
    for page in range(1, MAX_PAGES + 1):
        request = urllib.request.Request(
            page_url(api, repository, tool, ref, page),
            headers={'Accept': 'application/vnd.github+json', 'Authorization': 'Bearer ' + token,
                     'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'rdy-core-ci'})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                batch = json.loads(response.read().decode('utf-8'))
                # All Link fields: a response may carry more than one.
                links = response.headers.get_all('Link')
        except urllib.error.HTTPError as error:
            try:
                message = json.loads(error.read().decode('utf-8')).get('message', '')
            except (ValueError, AttributeError, OSError):
                message = ''
            finally:
                error.close()
            raise AlertQueryError('HTTP ' + str(error.code) + ' ' + clean(message)) from None
        except (urllib.error.URLError, OSError, ValueError, AttributeError) as error:
            raise AlertQueryError(clean(error)) from None
        if not isinstance(batch, list):
            raise AlertQueryError('the response is not an alert list')
        alerts.extend(batch)
        if not has_next(links) and len(batch) < PAGE_SIZE:
            return alerts
    raise AlertQueryError('more than ' + str(MAX_PAGES) + ' pages of alerts')


def alerts_main(tool, ref):
    if not re.fullmatch(r'refs/(heads|tags|pull)/[A-Za-z0-9_./-]+', ref or ''):
        raise AlertQueryError('the ref to check is missing or invalid')
    alerts = fetch_alerts(os.environ.get('GITHUB_API_URL', 'https://api.github.com'),
                          os.environ.get('GITHUB_REPOSITORY'), os.environ.get('GITHUB_TOKEN'), tool, ref)
    try:
        blocking = blocking_alerts(alerts, tool)
    except ValueError as error:
        raise AlertQueryError(str(error)) from None
    lines = [alert_line(alert) for alert in blocking]
    print(tool + ' on ' + ref + ': ' + str(len(alerts)) + ' open alert(s), '
          + str(len(blocking)) + ' high or critical.')
    for line in lines:
        print(line)
    write_summary('Open high and critical ' + tool + ' alerts (' + str(len(blocking)) + ')', lines)
    return int(bool(blocking))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest='command', required=True)
    sarif = commands.add_parser('sarif')
    sarif.add_argument('directory')
    alerts = commands.add_parser('alerts')
    alerts.add_argument('--tool', required=True)
    alerts.add_argument('--ref', required=True)
    args = parser.parse_args(argv)
    if args.command == 'sarif':
        return sarif_main(args.directory)
    return alerts_main(args.tool, args.ref)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except AlertQueryError as error:
        raise SystemExit('code-scanning alert query failed, so this gate cannot pass: ' + str(error))
    except (ValueError, KeyError, TypeError, OSError, IndexError, subprocess.CalledProcessError) as error:
        raise SystemExit('CodeQL result policy refused incomplete or invalid analysis: ' + clean(error))
