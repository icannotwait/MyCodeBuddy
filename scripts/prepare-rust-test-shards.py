#!/usr/bin/env python3
"""Regenerate test gates and the reviewable source inventory (native Linux spike).

Strips only the exact generated gate lines, parses the real lib module tree with
syn, keeps each source file in one shard, and balances source definition counts.
Run only when no Cargo/shard run is active. Review changes before using in CI.
"""
import argparse
import collections
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('shards', ROOT/'scripts/rust-test-shards.py')
shards = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shards)

def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()

def write_if_changed(path, text):
    if not path.exists() or path.read_text() != text:
        path.write_text(text)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--check', action='store_true', help='Verify regeneration is byte-identical without editing')
    parser.add_argument('--split-shard', type=int, help='Append a shard by dividing one existing file group in half')
    parser.add_argument('--isolate-file', help='With --split-shard, keep only this source file in the old group')
    args = parser.parse_args()
    if args.isolate_file and args.split_shard is None:
        parser.error('--isolate-file requires --split-shard')
    old = json.loads(shards.MANIFEST.read_text()) if shards.MANIFEST.exists() else {}
    if args.split_shard is not None:
        shards.audit_source(old)
    sources = {str(p.relative_to(ROOT)):shards.strip_gates(p.read_text()) for p in sorted((ROOT/'src-tauri/src').rglob('*.rs'))}
    with tempfile.TemporaryDirectory(prefix='codeg-test-inventory-') as temporary:
        temp = Path(temporary)
        for name, source in sources.items():
            path = temp/name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        command = ['cargo','run','--locked','--manifest-path',str(ROOT/'scripts/rust-test-inventory/Cargo.toml'),
                   '--target-dir',str(ROOT/'src-tauri/target/test-inventory')]
        if args.offline:
            command.append('--offline')
        command += ['--',str(temp/'src-tauri/src')]
        inventory = json.loads(subprocess.check_output(command,cwd=ROOT,text=True))
        if inventory['warnings']:
            raise ValueError(f'Unsupported or unregistered test source: {inventory["warnings"]}')
        for row in [*inventory['tests'],*inventory['gates']]:
            row['file'] = str(Path(row['file']).relative_to(temp))
    counts = collections.Counter(t['file'] for t in inventory['tests'])
    assignment = {row['file']:row['shard'] for row in old.get('files',[]) if row['file'] in counts}
    loads = [0]*len(old.get('shards',range(8)))
    for name, shard in assignment.items():
        loads[shard] += counts[name]
    for name, count in sorted(counts.items(),key=lambda item:(-item[1],item[0])):
        if name in assignment:
            continue
        shard = min(range(len(loads)),key=lambda value:(loads[value],value))
        assignment[name] = shard
        loads[shard] += count
    if args.split_shard is not None:
        original, added = args.split_shard, len(loads)
        group = sorted([(name,counts[name]) for name,shard in assignment.items() if shard==original],key=lambda item:(-item[1],item[0]))
        if len(group)<2 or added>=16:
            raise ValueError('This file-preserving split requires at least two files and fewer than 16 shards')
        loads[original] = 0
        loads.append(0)
        if args.isolate_file and args.isolate_file not in {name for name,_ in group}:
            raise ValueError('--isolate-file must belong to the selected existing shard')
        for name,count in group:
            shard = (original if name == args.isolate_file else added) if args.isolate_file else min((original,added),key=lambda value:(loads[value],value))
            assignment[name] = shard
            loads[shard] += count
    files, generated = [], dict(sources)
    for name in sorted(counts):
        lines = sources[name].splitlines(keepends=True)
        gates = [g for g in inventory['gates'] if g['file']==name]
        for test in inventory['tests']:
            if test['file']==name:
                test['shard'] = assignment[name]
                test['sha256'] = digest(''.join(lines[test['line']-1:test['end_line']]))
        for gate in sorted(gates,key=lambda row:row['line'],reverse=True):
            line = lines[gate['line']-1]
            if gate['column'] != len(line)-len(line.lstrip()):
                raise ValueError(f'Non-line-leading test item needs manual review: {gate}')
            lines.insert(gate['line']-1,' '*gate['column']+f'#[cfg(any(not(codeg_test_shard), codeg_test_shard = "{assignment[name]}"))]\n')
        generated[name] = ''.join(lines)
        files.append({'file':name,'shard':assignment[name],'tests':counts[name],
                      'gates':len(gates),'sha256':digest(sources[name])})
    baseline = subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
    # Preserve the recorded origin when only regenerating the same source set.
    hashes = {name:digest(source) for name,source in sources.items()}
    if old.get('source_files') == hashes:
        baseline = old['baseline_commit']
    inventory.update(schema=1,baseline_commit=baseline,shards=list(range(len(loads))),
                     source_test_counts=loads,files=files,source_files=hashes)
    generated['src-tauri/test-shard-count.txt'] = f'{len(loads)}\n'
    generated[str(shards.MANIFEST.relative_to(ROOT))] = json.dumps(inventory,indent=2)+'\n'
    changes = [name for name,text in generated.items() if not (ROOT/name).exists() or (ROOT/name).read_text()!=text]
    if args.check and changes:
        raise ValueError(f'Inventory/gates need regeneration: {changes}')
    if not args.check:
        for name,text in generated.items():
            write_if_changed(ROOT/name,text)
    print(f'{len(inventory["tests"])} source test definitions; shard sizes {loads}; {len(changes)} changed files')

if __name__=='__main__':
    main()
