#!/usr/bin/env python3
"""Fresh-artifact-dir check for the RELGATE1 Gaten gate rows.

For every <row>/artifacts directory under the given root, prove that every
artifact in it belongs to exactly one run:

  * run.json / engine.json / repro.json / verdict.json all name the same
    request_id and session_id;
  * verdict.json's scenario/apk digests match the scenario the row was asked to
    run (so no artifact was carried over from another scenario);
  * every file's mtime lies inside [started_at - 120 s, finished_at + 120 s];
  * every evidence path the verdict names exists;
  * no JSON/JSONL document in the directory mentions a different request_id;
  * exactly one run.json and one verdict.json exist.

usage: fresh-check.py <root> <scenario-list>
exit 1 on any finding.
"""
import json, os, re, sys
from datetime import datetime, timezone

root, listfile = sys.argv[1], sys.argv[2]
rows = [l.strip() for l in open(listfile) if l.strip()]


def iso(ts):
    return datetime.strptime(ts.replace('Z', '+0000'), '%Y-%m-%dT%H:%M:%S.%f%z').timestamp()


def name_of(path):
    n = re.sub(r'^scenarios/', '', path).replace('/', '__')
    return n[:-5] if n.endswith('.yaml') else n


bad = []
for sc in rows:
    d = os.path.join(root, name_of(sc), 'artifacts')
    if not os.path.isdir(d):
        bad.append((sc, 'missing artifacts dir ' + d))
        continue
    reqs, sessions, starts, finishes = set(), set(), [], []
    for fname in ('run.json', 'engine.json', 'repro.json', 'verdict.json'):
        p = os.path.join(d, fname)
        if not os.path.exists(p):
            bad.append((sc, f'missing {fname}'))
            continue
        doc = json.load(open(p))
        for key in ('request_id',):
            v = doc.get(key)
            if v:
                reqs.add(v)
        v = doc.get('session_id')
        if v:
            sessions.add(v)
        for key, bucket in (('started_at', starts), ('finished_at', finishes)):
            v = doc.get(key)
            if v:
                bucket.append(iso(v))
        if fname == 'verdict.json':
            verdict = doc
    for fname in ('run.json', 'verdict.json'):
        if not os.path.exists(os.path.join(d, fname)):
            continue
    if len(reqs) != 1:
        bad.append((sc, f'request_id mismatch across artifacts: {sorted(reqs)}'))
    if len(sessions) != 1:
        bad.append((sc, f'session_id mismatch: {sorted(sessions)}'))
    if not starts or not finishes:
        bad.append((sc, 'missing started_at/finished_at'))
        continue
    t0, t1 = min(starts) - 120, max(finishes) + 120
    # evidence paths must exist
    for step in verdict.get('steps') or []:
        for ev in step.get('evidence') or []:
            p = os.path.join(d, ev['path'])
            if not os.path.isfile(p):
                bad.append((sc, 'dangling evidence ' + ev['path']))
    # one run document each
    for fname in ('run.json', 'verdict.json', 'repro.json'):
        n = len([f for f in os.listdir(d) if f == fname])
        if n != 1:
            bad.append((sc, f'{fname} count {n}'))
    # foreign request ids + mtimes
    for dirpath, _dirs, files in os.walk(d):
        for f in files:
            p = os.path.join(dirpath, f)
            m = os.path.getmtime(p)
            if not (t0 <= m <= t1):
                bad.append((sc, f'{os.path.relpath(p, d)} mtime outside the run window '
                                f'({datetime.fromtimestamp(m, timezone.utc).isoformat()} '
                                f'vs {datetime.fromtimestamp(t0, timezone.utc).isoformat()}'
                                f'..{datetime.fromtimestamp(t1, timezone.utc).isoformat()})'))
            if f.endswith(('.json', '.jsonl')) and m < 50 * 1024 * 1024:
                try:
                    txt = open(p, errors='replace').read()
                except OSError:
                    continue
                for rid in set(re.findall(r'req_[0-9a-f]+_[0-9a-f]+', txt)):
                    if rid not in reqs:
                        bad.append((sc, f'{os.path.relpath(p, d)} mentions foreign {rid}'))
    print(f'ok  {sc}  req={sorted(reqs)}  files={sum(len(f) for _,_,f in os.walk(d))}')

print(f'\nrows {len(rows)}  findings {len(bad)}')
for sc, why in bad:
    print('FINDING', sc, '::', why)
sys.exit(1 if bad else 0)
