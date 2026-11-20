#!/usr/bin/env python3
"""Classify a RELGATE1 scenario sweep against each scenario's declared expectation.

For every scenario in LIST, read <out>/<name>.json (the CLI --json reply) and
<out>/<name>/artifacts/verdict.json, and decide OK / FAIL / MISMATCH.

Rules (mirroring crates/acos-verify/src/executor.rs apply_expectation):
  expectation pass      -> OK iff exit 0 and status == "pass"
  expectation fail      -> OK iff status == "fail" (an "error"/"unsupported" is
                           not the declared failure), and, when expect_error is
                           declared, the code (and reason when given) match exactly
  expectation non_pass  -> OK iff status != "pass" (and expect_error matches when declared)
  expectation unsupported -> OK iff status == "unsupported"
A scenario with no expectation defaults to pass.
"""
import json, os, re, sys

LIST, OUT = sys.argv[1], sys.argv[2]


def yaml_head(path):
    d = {}
    for line in open(path):
        if line.startswith(('#',)) or not line.strip():
            continue
        m = re.match(r'^([a-z_]+):\s*(.*)$', line.rstrip('\n'))
        if m and m.group(1) not in d:
            v = m.group(2).strip()
            d[m.group(1)] = v
        if line.startswith('steps:'):
            break
    return d


def expect_error(path):
    """Parse the inline `expect_error: {code: X, reason: y}` mapping (one line)."""
    txt = open(path).read()
    m = re.search(r'^expect_error:\s*(.+)$', txt, re.M)
    if not m:
        return None, None
    raw = m.group(1).strip()
    code = re.search(r'code:\s*([A-Z_0-9]+)', raw)
    reason = re.search(r'reason:\s*"?([A-Za-z0-9_\-]+)"?', raw)
    return (code.group(1) if code else None), (reason.group(1) if reason else None)


rows = {}
rows_path = os.path.join(OUT, 'rows.jsonl')
if os.path.exists(rows_path):
    for line in open(rows_path):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        rows[r['scenario']] = r

out = []
for path in [l.strip() for l in open(LIST) if l.strip()]:
    name = re.sub(r'^scenarios/', '', path)
    name = name.replace('/', '__')
    if name.endswith('.yaml'):
        name = name[:-5]
    head = yaml_head(path)
    exp = head.get('expectation', 'pass').strip()
    ecode, ereason = expect_error(path)
    row = rows.get(path, {})
    jp = os.path.join(OUT, name + '.json')
    status = code = reason = None
    steps = None
    vpath = None
    err = ''
    if os.path.exists(jp):
        try:
            d = json.load(open(jp))
            status = d.get('status')
            err = d.get('error') or {}
            code = err.get('code') if isinstance(err, dict) else None
            details = (err.get('details') or {}) if isinstance(err, dict) else {}
            reason = details.get('reason')
            res = d.get('result') or {}
            vpath = res.get('verdict_path')
            if not vpath:
                vp = os.path.join(OUT, name, 'artifacts', 'verdict.json')
                vpath = vp if os.path.exists(vp) else None
            if vpath and os.path.exists(vpath):
                v = json.load(open(vpath))
                steps = len(v.get('steps') or [])
                if steps is None:
                    steps = None
        except Exception as exc:  # noqa: BLE001
            status = 'UNPARSED'
            err = {'code': repr(exc)}
    exit_code = row.get('exit')
    if exit_code is None:
        exit_code = 'NORUN'

    ok = False
    why = ''
    if status is None:
        ok, why = False, 'no reply json'
    elif exp == 'pass':
        ok = (exit_code == 0 and status == 'pass')
        why = '' if ok else f'expected pass, got {status}'
    elif exp == 'fail':
        if status == 'fail':
            ok = True
        else:
            ok, why = False, f'expected fail, got {status}'
    elif exp == 'non_pass':
        ok = status not in ('pass',)
        why = '' if ok else 'expected non_pass, got pass'
    elif exp == 'unsupported':
        ok = status == 'unsupported'
        why = '' if ok else f'expected unsupported, got {status}'
    else:
        ok, why = False, f'unknown expectation {exp!r}'
    if ok and ecode:
        if code != ecode or (ereason and reason != ereason):
            ok = False
            why = f'expected fail as {ecode}/{ereason}, got {code}/{reason}'
    out.append({
        'scenario': path, 'expectation': exp, 'expect_error': f'{ecode}/{ereason}' if ecode else '',
        'got_status': status, 'error_code': code, 'reason': reason, 'steps': steps,
        'exit': exit_code, 'secs': row.get('secs'), 'verdict': vpath, 'ok': ok, 'why': why,
    })

bad = [r for r in out if not r['ok']]
print(f'total {len(out)}  ok {len(out)-len(bad)}  bad {len(bad)}')
for r in bad:
    print(f"BAD {r['scenario']} exp={r['expectation']} {r['expect_error']} got={r['got_status']} "
          f"code={r['error_code']} reason={r['reason']} exit={r['exit']} secs={r['secs']} :: {r['why']}")
json.dump(out, open(os.path.join(OUT, 'table.json'), 'w'), indent=1)
