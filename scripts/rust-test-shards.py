#!/usr/bin/env python3
"""Experimental, fail-closed compilation sharding for the unchanged core lib tests.

Default runs every shard. --shard is an explicitly incomplete diagnostic pilot.
No-default-features + test-utils only; does not replace integrations or desktop CI.
"""
import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'scripts/rust-test-shards-manifest.json'
GATE = re.compile(r'^[ \t]*#\[cfg\(any\(not\(codeg_test_shard\), codeg_test_shard = "(\d+)"\)\)\]\n', re.M)
ACTIVE_RESULT = {}

TOKEN = re.compile(r'\s*("(?:[^"\\]|\\.)*"|[A-Za-z_][A-Za-z_0-9]*|[(),=])')

def strip_gates(source):
    return GATE.sub('', source)

def eval_cfg(text, cfg):
    tokens, pos = [], 0
    while pos < len(text):
        match = TOKEN.match(text, pos)
        if not match:
            if not text[pos:].strip():
                break
            raise ValueError(f'Unsupported cfg syntax: {text}')
        tokens.append(match.group(1))
        pos = match.end()
    index = 0
    def parse():
        nonlocal index
        if index >= len(tokens):
            raise ValueError(f'Truncated cfg: {text}')
        name = tokens[index]
        index += 1
        if index < len(tokens) and tokens[index] == '(':
            index += 1
            args = []
            while index < len(tokens) and tokens[index] != ')':
                args.append(parse())
                if index < len(tokens) and tokens[index] == ',':
                    index += 1
                elif index < len(tokens) and tokens[index] != ')':
                    raise ValueError(f'Missing comma in cfg: {text}')
            if index >= len(tokens):
                raise ValueError(f'Unclosed cfg: {text}')
            index += 1
            if name == 'all':
                return all(args)
            if name == 'any':
                return any(args)
            if name in ('cfg', 'not') and len(args) == 1:
                return args[0] if name == 'cfg' else not args[0]
            raise ValueError(f'Unsupported cfg operation: {text}')
        if index < len(tokens) and tokens[index] == '=':
            index += 1
            if index >= len(tokens) or not tokens[index].startswith('"'):
                raise ValueError(f'Invalid cfg value: {text}')
            name += '=' + tokens[index]
            index += 1
        return name in cfg
    result = parse()
    if index != len(tokens):
        raise ValueError(f'Trailing cfg tokens: {text}')
    return result

def audit_names(expected, actual):
    wanted, got = collections.Counter(expected), collections.Counter(actual)
    duplicate_expected = [name for name, count in wanted.items() if count != 1]
    duplicate_actual = [name for name, count in got.items() if count != 1]
    if duplicate_expected or duplicate_actual or wanted != got:
        raise ValueError(json.dumps({'missing': sorted((wanted-got).elements()),
                                     'unexpected': sorted((got-wanted).elements()),
                                     'duplicate_expected': duplicate_expected,
                                     'duplicate_actual': duplicate_actual}, indent=2))

def parse_listing(text):
    names = []
    for line in text.splitlines():
        if line.endswith(': test'):
            names.append(line[:-6])
        elif line.strip() and not re.fullmatch(r'\d+ tests?, \d+ benchmarks?', line):
            raise ValueError(f'Unexpected test listing line: {line}')
    return names

def parse_execution(text, expected_count):
    matches = re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', text)
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous final execution accounting')
    result = dict(zip(('passed','failed','ignored','measured','filtered_out'), map(int,matches[0])))
    if result['measured'] or result['filtered_out'] or sum(result[k] for k in ('passed','failed','ignored')) != expected_count:
        raise ValueError(f'Execution does not cover the listed tests: {result}')
    return result

def audit_source(manifest, root=ROOT):
    current = {str(p.relative_to(root)):p for p in (root/'src-tauri/src').rglob('*.rs')}
    if set(current) != set(manifest['source_files']):
        raise ValueError('Rust source files changed; regenerate and independently review the shard inventory')
    actual_gates = []
    for name, path in current.items():
        source = path.read_text()
        prior_gates = 0
        for line_number, line in enumerate(source.splitlines(keepends=True), start=1):
            match = GATE.fullmatch(line)
            if match:
                actual_gates.append((name,line_number-prior_gates,line.index('#'),int(match.group(1))))
                prior_gates += 1
        digest = hashlib.sha256(strip_gates(source).encode()).hexdigest()
        if digest != manifest['source_files'][name]:
            raise ValueError(f'Source differs from the inventoried baseline: {name}')
    assignment = {row['file']:row['shard'] for row in manifest['files']}
    expected_gates = [(row['file'],row['line'],row['column'],assignment[row['file']]) for row in manifest['gates']]
    if collections.Counter(actual_gates) != collections.Counter(expected_gates):
        raise ValueError('Generated test gates moved, disappeared, or appeared outside their inventoried test locations')
    for row in manifest['files']:
        gates = GATE.findall(current[row['file']].read_text())
        if len(gates) != row['gates'] or any(int(s) != row['shard'] for s in gates):
            raise ValueError(f'Missing or wrong shard gate: {row["file"]}')
    count = int((root/'src-tauri/test-shard-count.txt').read_text().strip())
    if not 1 <= count <= 16 or manifest['shards'] != list(range(count)):
        raise ValueError('Runner/build.rs shard set mismatch')

def audit_run_inputs(manifest, summary, root=ROOT):
    audit_source(manifest, root)
    for name, digest in summary['build_inputs_sha256'].items():
        if hashlib.sha256((root/name).read_bytes()).hexdigest() != digest:
            raise ValueError(f'Build input changed during the shard run: {name}')
    if hashlib.sha256((root/'scripts/rust-test-shards-manifest.json').read_bytes()).hexdigest() != summary['manifest_sha256']:
        raise ValueError('Shard manifest changed during the run')

def oom_kills():
    try:
        return int(next(line.split()[1] for line in Path('/proc/vmstat').read_text().splitlines() if line.startswith('oom_kill ')))
    except (OSError, StopIteration):
        return None

def run_logged(command, env, stdout, stderr):
    started, peak, before = time.monotonic(), 0, oom_kills()
    last_report = started
    with open(stdout, 'w') as out, open(stderr, 'w') as err:
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=out, stderr=err)
        while process.poll() is None:
            # No concurrent builds: sampled compiler/linker high-water RSS.
            for status in Path('/proc').glob('[0-9]*/status'):
                try:
                    fields = dict(line.split(':', 1) for line in status.read_text().splitlines() if ':' in line)
                    if fields.get('Name', '').strip() in ('rustc', 'cargo', 'cc', 'ld', 'rust-lld', 'collect2'):
                        peak = max(peak, int(fields.get('VmHWM', '0').split()[0]))
                except (OSError, ValueError, ProcessLookupError):
                    pass
            if time.monotonic()-last_report >= 60:
                print(f'Build still running: {int(time.monotonic()-started)}s; peak sampled process {peak/1024:.0f} MiB', flush=True)
                last_report = time.monotonic()
            time.sleep(1)
    return process.returncode, round(time.monotonic()-started, 3), peak, before, oom_kills()

def save_summary(output, summary):
    temporary = output/'summary.json.tmp'
    temporary.write_text(json.dumps(summary,indent=2)+'\n')
    temporary.replace(output/'summary.json')

def verify_native_scope(env):
    if sys.platform != "linux":
        raise ValueError("This experimental runner currently supports native Linux only")
    unsupported = [key for key in env if key in ('CARGO_BUILD_TARGET','RUSTC','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS') and env[key]]
    unsupported += [key for key in env if key.startswith('CARGO_TARGET_') and key.endswith(('_RUNNER','_RUSTFLAGS')) and env[key]]
    if unsupported:
        raise ValueError(f'Native-only runner does not support these overrides: {unsupported}')
    paths = [parent/'.cargo'/name for parent in (ROOT, *ROOT.parents) for name in ('config','config.toml')]
    home = Path(env.get('CARGO_HOME', str(Path.home()/'.cargo')))
    paths.extend(home/name for name in ('config','config.toml'))
    for path in paths:
        if not path.is_file():
            continue
        config = tomllib.loads(path.read_text())
        if config.get('build',{}).get('target') or config.get('build',{}).get('rustflags') or any(values.get('runner') or values.get('rustflags') for values in config.get('target',{}).values() if isinstance(values,dict)):
            raise ValueError(f'Native-only runner does not support target/rustflags/runner configuration in {path}')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--shard', type=int, choices=range(16), help='INCOMPLETE pilot: one shard only')
    parser.add_argument('--audit-only', action='store_true')
    parser.add_argument('--list-only', action='store_true', help='Build and audit; do not execute tests')
    parser.add_argument('--output', type=Path, default=ROOT/'src-tauri/target/shard-results')
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--offline', action='store_true')
    args = parser.parse_args()
    output = args.output.resolve()
    if not args.audit_only:
        output.mkdir(parents=True, exist_ok=True)
        initial = {'status':'running', 'success':False, 'complete_core_suite_passed':False,
                   'started_at_unix':time.time(), 'shards':[]}
        ACTIVE_RESULT.update(output=output, summary=initial)
        save_summary(output, initial)
    if 'CODEG_TEST_SHARD' in os.environ:
        raise ValueError('Unset CODEG_TEST_SHARD; this wrapper owns complete shard selection')
    verify_native_scope(os.environ)
    manifest = json.loads(MANIFEST.read_text())
    audit_source(manifest)
    env = os.environ.copy()
    env.update(CARGO_BUILD_JOBS='1', CARGO_INCREMENTAL='0', CARGO_PROFILE_TEST_DEBUG='0',
               CARGO_PROFILE_TEST_OPT_LEVEL='0', CARGO_PROFILE_TEST_INCREMENTAL='false',
               RUST_TEST_THREADS='1', RUST_MIN_STACK='33554432')
    cfg = set(subprocess.check_output(['rustc','--print','cfg'], env=env, text=True).splitlines())
    cfg.update({'test','feature="test-utils"'})
    expected = [t for t in manifest['tests'] if all(eval_cfg(c,cfg) for c in t['cfg'])]
    audit_names([t['name'] for t in expected], [t['name'] for t in expected])
    print(f'Source audit: {len(manifest["tests"])} total library test definitions, {len(expected)} active in this core feature/target scope', flush=True)
    if args.audit_only:
        return 0
    if args.shard is not None and args.shard not in manifest['shards']:
        raise ValueError('Requested shard is absent from the reviewed manifest')
    selected = manifest['shards'] if args.shard is None else [args.shard]
    complete = args.shard is None
    print(f'ALL {len(selected)} SHARDS' if complete else 'INCOMPLETE SINGLE-SHARD PILOT', flush=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    summary = {'baseline_commit':manifest['baseline_commit'], 'source_tests':len(manifest['tests']),
               'scope':'--no-default-features --features test-utils --lib', 'cfg':sorted(cfg),
               'expected_active_tests':len(expected), 'selected_shards':selected,
               'complete_selection':complete, 'list_only':args.list_only, 'shards':[],
               'status':'running', 'success':False, 'complete_core_suite_passed':False,
               'started_at_unix':ACTIVE_RESULT['summary']['started_at_unix']}
    summary['manifest_sha256'] = hashlib.sha256(MANIFEST.read_bytes()).hexdigest()
    summary['build_inputs_sha256'] = {name:hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in ('src-tauri/Cargo.toml','src-tauri/Cargo.lock','src-tauri/build.rs','src-tauri/test-shard-count.txt','.cargo/config.toml')}
    (output/'manifest.json').write_bytes(MANIFEST.read_bytes())
    ACTIVE_RESULT['summary'] = summary
    save_summary(output, summary)
    (output/'expected-tests.json').write_text(json.dumps(expected,indent=2)+'\n')
    seen, failure = [], False
    for shard in selected:
        audit_run_inputs(manifest, summary)
        summary.update(active_shard=shard,phase='building')
        save_summary(output, summary)
        print(f'Building shard {shard}/{len(manifest["shards"])-1}...', flush=True)
        env['CODEG_TEST_SHARD'] = str(shard)
        command = ['cargo','test','--locked','--manifest-path','src-tauri/Cargo.toml',
                   '--no-default-features','--features','test-utils','--lib','--no-run',
                   '--config','profile.test.package.codeg.codegen-units=256','--message-format=json']
        if args.offline:
            command.append('--offline')
        if args.target_dir:
            command.extend(['--target-dir',str(args.target_dir.resolve())])
        time_log = output/f'shard-{shard}.time.txt'
        measured = ['/usr/bin/time','-v','-o',str(time_log),*command] if Path('/usr/bin/time').exists() else command
        build_json, build_err = output/f'shard-{shard}.build.jsonl', output/f'shard-{shard}.build.stderr'
        code, seconds, peak, oom_before, oom_after = run_logged(measured, env, build_json, build_err)
        row = {'shard':shard,'command':command,'build_exit':code,'build_seconds':seconds, 'peak_sampled_process_kib':peak, 'oom_before':oom_before, 'oom_after':oom_after}
        if time_log.exists():
            match = re.search(r'Maximum resident set size \(kbytes\): (\d+)',time_log.read_text())
            row['max_child_rss_kib'] = int(match.group(1)) if match else None
        summary['shards'].append(row)
        summary['phase'] = 'built'
        save_summary(output, summary)
        if code:
            failure = True
            print(f'Shard {shard} build failed: see {build_err}', flush=True)
        else:
            executables = []
            for line in build_json.read_text().splitlines():
                event = json.loads(line)
                if event.get('reason')=='compiler-artifact' and event.get('target',{}).get('name')=='codeg_lib' and event.get('profile',{}).get('test') and event.get('executable'):
                    executables.append(event['executable'])
            if len(executables)!=1:
                raise ValueError(f'Expected exactly one library harness, got {executables}')
            executable = executables[0]
            listing = subprocess.check_output([executable,'--list','--format','terse'], cwd=ROOT/'src-tauri', env=env,text=True)
            (output/f'shard-{shard}.list.txt').write_text(listing)
            names = parse_listing(listing)
            audit_names([t['name'] for t in expected if t['shard']==shard], names)
            seen.extend(names)
            with open(executable, 'rb') as binary:
                binary_hash = hashlib.file_digest(binary, 'sha256').hexdigest()
            row.update(tests=len(names),name_audit='passed',executable=executable,
                       executable_sha256=binary_hash, executable_retained=False)
            summary['phase'] = 'listed'
            save_summary(output, summary)
            print(f'Shard {shard}: {len(names)} test names match inventory', flush=True)
            if not args.list_only:
                summary['phase'] = 'executing'
                save_summary(output, summary)
                started=time.monotonic()
                with open(output/f'shard-{shard}.tests.log','w') as log:
                    result=subprocess.run([executable,'--test-threads=1'],cwd=ROOT/'src-tauri',env=env,stdout=log,stderr=subprocess.STDOUT)
                row.update(test_exit=result.returncode,test_seconds=round(time.monotonic()-started,3))
                row['execution'] = parse_execution((output/f'shard-{shard}.tests.log').read_text(), len(names))
                failure |= result.returncode!=0 or row['execution']['failed']!=0
                print(f'Shard {shard} execution exit {result.returncode}', flush=True)
        save_summary(output, summary)
    audit_run_inputs(manifest, summary)
    wanted=[t['name'] for t in expected if t['shard'] in selected]
    try:
        audit_names(wanted,seen)
        summary['union_audit']='passed'
    except ValueError as error:
        summary['union_audit']=str(error)
        failure=True
    summary.update(active_shard=None, phase='finished', status='failed' if failure else 'completed', compiled_test_count=len(seen), success=not failure,
                   complete_core_suite_passed=complete and not args.list_only and not failure)
    save_summary(output, summary)
    print(json.dumps({k:summary[k] for k in ('compiled_test_count','success','complete_core_suite_passed')},indent=2))
    return 1 if failure else 0

if __name__ == '__main__':
    try:
        sys.exit(main())
    except (Exception, KeyboardInterrupt) as error:
        if ACTIVE_RESULT:
            summary = ACTIVE_RESULT['summary']
            summary.update(status='failed', success=False, complete_core_suite_passed=False, error=str(error))
            save_summary(ACTIVE_RESULT['output'], summary)
        print(f'Shard audit failed: {error}',file=sys.stderr)
        sys.exit(1)
