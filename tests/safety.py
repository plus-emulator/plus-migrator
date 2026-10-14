#!/usr/bin/env python3
"""Exercise rejection paths on a local disposable MariaDB rehearsal, without writes.

Uses a local test account without a password, and never prints table contents.
The fixture must contain compatibility blockers (as the production rehearsal does).
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--mysql', default='mariadb')
p.add_argument('--socket', required=True)
p.add_argument('--port', required=True, type=int)
p.add_argument('--user', default='root')
p.add_argument('--source', default='arc_source')
p.add_argument('--target', default='plus_test')
p.add_argument('--binary', required=True)
p.add_argument('--validator', required=True)
p.add_argument('--artifacts', required=True)
a = p.parse_args()
for value in (a.source, a.target, a.user):
    if not re.fullmatch(r'[A-Za-z0-9_]+', value):
        p.error('use simple test database/account names')
out = Path(a.artifacts)
out.mkdir(mode=0o700)  # Refuse to overwrite a previous test's evidence.

def query(sql):
    return subprocess.check_output([a.mysql, '--no-defaults', '--socket='+a.socket,
                                    '--user='+a.user, '-NB', '-e', sql], text=True)

def checksums(db):
    tables = query(f'SHOW FULL TABLES FROM `{db}` WHERE Table_type="BASE TABLE"').splitlines()
    names = ','.join(f'`{db}`.`'+r.split('\t')[0].replace('`', '``')+'`' for r in tables)
    return query('CHECKSUM TABLE '+names+' EXTENDED')

before = {db: checksums(db) for db in (a.source, a.target)}
source = f'mysql://{a.user}@127.0.0.1:{a.port}/{a.source}'
target = f'mysql://{a.user}@127.0.0.1:{a.port}/{a.target}'
base = [a.binary, '--source', source, '--source-offline', '--dry-run', '--force']
results = {}
for name, extra in [
    ('strict_preflight', ['--target', target, '--wired-validator', a.validator]),
    ('same_database_alias', ['--target', f'mysql://{a.user}@localhost:{a.port}/{a.source}']),
    ('partial_run', ['--target', target, '--only', 'users']),
    ('invalid_validator', ['--target', target, '--wired-validator', '/bin/true']),
]:
    report = out/(name+'.json')
    with (out/(name+'.log')).open('w') as log:
        result = subprocess.run(base+extra+['--report', str(report)], stdout=log, stderr=log)
    results[name] = result.returncode
    assert result.returncode != 0, name+' unexpectedly succeeded'
    if name == 'strict_preflight':
        body = json.loads(report.read_text())
        assert body['wired_records_checked'] == body['wired_records_generated'] > 0
    if name == 'invalid_validator':
        body = json.loads(report.read_text())
        assert body['wired_records_checked'] == 0
        assert any('Wired validator failed' in text for text in body['blockers'])
for db in before:
    results[db+'_unchanged'] = before[db] == checksums(db)
    assert results[db+'_unchanged'], db+' changed during rejected preflight'
(out/'result.json').write_text(json.dumps(results, indent=2))
print(json.dumps(results))
