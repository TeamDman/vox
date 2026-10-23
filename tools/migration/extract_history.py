"""Extract a directory into a NEW ref; retain original commits and map every rewrite.

Uses Git's path-limited fast-export (including history simplification), strips only
the directory prefix from file commands, and verifies every resulting tree.
Never alters the source ref or rewrites an existing destination ref.
"""
import argparse
import json
import pathlib
import subprocess

p = argparse.ArgumentParser()
p.add_argument('repo')
p.add_argument('source')
p.add_argument('prefix')
p.add_argument('destination')
p.add_argument('audit_directory')
a = p.parse_args()
repo = pathlib.Path(a.repo).resolve()
audit = pathlib.Path(a.audit_directory).resolve()
audit.mkdir(parents=True, exist_ok=True)

def git(*args, **kwargs):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, **kwargs)

if subprocess.run(['git', '-C', str(repo), 'show-ref', '--verify', '--quiet', a.destination]).returncode == 0:
    raise SystemExit('Ref already exists; refusing to replace it: ' + a.destination)
source_ref = 'refs/heads/migration/extraction-source-' + a.prefix.replace('/', '-') + '-' + a.source[:12]
if subprocess.run(['git', '-C', str(repo), 'show-ref', '--verify', '--quiet', source_ref]).returncode == 0:
    existing = git('rev-parse', source_ref, stdout=subprocess.PIPE).stdout
    requested = git('rev-parse', a.source, stdout=subprocess.PIPE).stdout
    if existing != requested:
        raise SystemExit('Extraction source ref differs from requested source: ' + source_ref)
else:
    git('branch', source_ref.removeprefix('refs/heads/'), a.source)
raw = audit / 'export.fast-export'
transformed = audit / 'import.fast-export'
marks = audit / 'import.marks'
with raw.open('wb') as output:
    git('fast-export', '--no-data', '--show-original-ids', '--signed-tags=strip', '--signed-commits=strip',
        '--use-done-feature', '--full-tree', '--no-renames', '--refspec=' + source_ref + ':' + a.destination,
        source_ref, '--', '--', a.prefix + '/', stdout=output)

prefix = a.prefix.encode() + b'/'
old_by_mark = {}
current_mark = None
with raw.open('rb') as source, transformed.open('wb') as output:
    while line := source.readline():
        if line.startswith(b'mark '):
            current_mark = line.split()[1].decode()
        elif line.startswith(b'original-oid '):
            old_by_mark[current_mark] = line.split()[1].decode()
        if line.startswith(b'data '):
            output.write(line)
            size = int(line.split()[1])
            data = source.read(size)
            if len(data) != size:
                raise RuntimeError('Truncated fast-export data')
            output.write(data)
            continue
        if line.startswith((b'M ', b'D ')):
            fields = line.rstrip(b'\n').split(b' ', 3 if line.startswith(b'M ') else 1)
            path = fields[-1]
            if path.startswith(prefix):
                fields[-1] = path[len(prefix):]
            elif path.startswith(b'"' + prefix):
                fields[-1] = b'"' + path[len(prefix) + 1:]
            elif path == a.prefix.encode() and line.startswith(b'D '):
                output.write(b'deleteall\n')
                continue
            else:
                raise RuntimeError('Unexpected file outside prefix: ' + repr(path))
            line = b' '.join(fields) + b'\n'
        if line.startswith((b'R ', b'C ')):
            raise RuntimeError('Unexpected rename/copy command')
        output.write(line)
with transformed.open('rb') as source:
    git('fast-import', '--quiet', '--export-marks=' + str(marks), stdin=source)
new_by_mark = dict(line.split() for line in marks.read_text().splitlines())
mapping = [{'original': old, 'extracted': new_by_mark[mark]} for mark, old in old_by_mark.items()]
queries = ''.join(row['original'] + ':' + a.prefix + '\n' + row['extracted'] + '^{tree}\n' for row in mapping)
result = git('cat-file', '--batch-check=%(objectname) %(objecttype)', input=queries.encode(), stdout=subprocess.PIPE)
trees = result.stdout.decode().splitlines()
for i, row in enumerate(mapping):
    original_tree, extracted_tree = trees[2*i:2*i+2]
    if original_tree != extracted_tree or not original_tree.endswith(' tree'):
        raise RuntimeError(f'Tree verification failed: {row}: {original_tree} != {extracted_tree}')
    row['tree'] = original_tree.split()[0]
receipt = {'source': a.source, 'prefix': a.prefix, 'destination': a.destination,
           'tip': git('rev-parse', a.destination, stdout=subprocess.PIPE).stdout.decode().strip(),
           'verified_commit_count': len(mapping), 'commits': mapping}
(audit / 'commit-map.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({k:v for k,v in receipt.items() if k != 'commits'}))
