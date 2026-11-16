from pathlib import Path
import hashlib
import json
import re
import sys
import subprocess
from datetime import datetime, timezone

round_root = Path(__file__).resolve().parent
repo = round_root.parents[3]
attempt = sys.argv[1] if len(sys.argv) > 1 else 'final-attempt1'
gate_root = round_root / attempt
snapshots = {}

def read_snapshot(path):
    if path not in snapshots:
        snapshots[path] = path.read_bytes()
    return snapshots[path]

manifest = json.loads(read_snapshot(gate_root / 'manifest.json').decode('utf-8-sig'))
assert len(manifest['gates']) == 6 and all(g['exit_code'] == 0 for g in manifest['gates'])
assert manifest['product_tree'] == manifest['final_product_tree']
assert [g['name'] for g in manifest['gates']] == ['check', 'debug-bins', 'test', 'fmt', 'clippy', 'wasm-check']

def git(*args):
    return subprocess.check_output(['git', '-C', str(repo), *args]).decode('utf-8').strip()

def validate_inputs():
    assert not git('status', '--porcelain', '--', 'NoManCode'), 'Product inputs are dirty'
    for path, field in [('NoManCode', 'product_tree'), ('NoManCode/rust-app', 'rust_tree')]:
        assert git('rev-parse', 'HEAD:' + path) == manifest[field], 'Current tree differs from tested tree'
        assert git('rev-parse', manifest['head'] + ':' + path) == manifest[field], 'Tested commit tree mismatch'

validate_inputs()
for recorded_manifest in [gate_root / 'manifest.json', *[p for p in sorted(round_root.glob('final-attempt*/manifest.json')) if p.parent != gate_root]]:
    recorded = json.loads(read_snapshot(recorded_manifest).decode('utf-8-sig'))
    for gate in recorded['gates']:
        raw = read_snapshot(recorded_manifest.parent / (gate['name'] + '.log'))
        assert len(raw) == gate['bytes'] and hashlib.sha256(raw).hexdigest() == gate['sha256'].lower(), f'Gate log mismatch: {recorded_manifest.parent.name}/{gate["name"]}'
for item in json.loads(read_snapshot(round_root / 'intermediate-results.json').decode('utf-8-sig')):
    raw = read_snapshot(round_root / (item['name'] + '.log'))
    assert len(raw) == item['bytes'] and hashlib.sha256(raw).hexdigest() == item['sha256'].lower(), f'Intermediate log mismatch: {item["name"]}'
destination = repo / 'niuma/项目经理/审查记录/2026-11-16深审修复/验证'
destination.mkdir(parents=True, exist_ok=False)

def sha(data):
    return hashlib.sha256(data).hexdigest()

def normalized(raw):
    text = raw.decode('utf-8-sig')
    replacements = [
        (str(repo), '../nhc-audit-20261116-integration'),
        (repo.as_posix(), '../nhc-audit-20261116-integration'),
    ]
    for source, target in replacements:
        text = text.replace('\\\\?\\' + source, target).replace(source, target)
    return text.encode('utf-8')

records = []
sources = [(p, '专项-' + p.name) for p in sorted(round_root.glob('*.log'))]
sources += [(p, '最终-' + p.name) for p in sorted(gate_root.glob('*.log'))]
for previous in sorted(round_root.glob('final-attempt*')):
    if previous == gate_root or not previous.is_dir():
        continue
    sources += [(p, previous.name + '-' + p.name) for p in sorted(previous.glob('*')) if p.suffix in ('.log', '.json')]
sources += [(round_root / 'intermediate-results.json', '中途验证.json'),
            (gate_root / 'manifest.json', '最终门禁.json'),
            (round_root / 'window-closed.json', '窗口收尾.json')]
sources += [(round_root / name, name) for name in ['run-gates-active-attempt2.ps1', 'run-gates.ps1', 'publish_evidence.py']]
for source, name in sources:
    raw = read_snapshot(source)
    published = normalized(raw)
    (destination / name).write_bytes(published)
    records.append({
        'source': '../nhc-audit-20261116-integration/' + source.relative_to(repo).as_posix(),
        'published': name,
        'source_sha256': sha(raw), 'source_bytes': len(raw),
        'published_sha256': sha(published), 'published_bytes': len(published),
    })

test_text = read_snapshot(gate_root / 'test.log').decode('utf-8-sig')
blocks = re.findall(r'^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;', test_text, re.M)
assert blocks
totals = {'passed': sum(int(b[1]) for b in blocks),
          'failed': sum(int(b[2]) for b in blocks),
          'ignored': sum(int(b[3]) for b in blocks),
          'result_blocks': len(blocks)}
assert totals['failed'] == 0
mock_text = read_snapshot(round_root / 'build-script-tests.log').decode('utf-8-sig')
mock_count = len(re.findall(r'^PASS ', mock_text, re.M))
assert mock_count == 13
validate_inputs()
summary = {
    'candidate': manifest['head'], 'product_tree': manifest['product_tree'],
    'rust_tree': manifest['rust_tree'], 'started_utc': manifest['started_utc'],
    'ended_utc': manifest['ended_utc'], 'test_threads': 1,
    'rust_test_result_totals': totals,
    'web_assertion_groups': len(re.findall(r'^PASS ', test_text, re.M)),
    'build_mock_scenarios': mock_count,
    'previous_attempts': [p.name for p in sorted(round_root.glob('final-attempt*')) if p.is_dir() and p != gate_root],
    'normalization': 'Only the integration worktree prefix is made repository-relative; source bytes remain untouched.',
    'acceptance_check': {
        'verified_utc': datetime.now(timezone.utc).isoformat(), 'current_head': git('rev-parse', 'HEAD'),
        'product_inputs_clean': True, 'current_and_tested_trees_match': True,
        'recorded_gate_and_intermediate_hashes_match': True,
        'runner_note': 'Attempt2 used run-gates-active-attempt2.ps1. Final publication independently rechecked clean inputs, tested/current trees, and every recorded log hash; run-gates.ps1 contains the later final-manifest ordering fix.'
    },
    'gates': manifest['gates'], 'files': records,
}
(destination / '证据清单.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(json.dumps({k: v for k, v in summary.items() if k not in ('gates', 'files')}, ensure_ascii=False, indent=2))
