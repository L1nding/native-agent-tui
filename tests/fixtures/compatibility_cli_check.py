"""Verify compatibility refusal, query cleanup, and read-only replay via the CLI."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def values(path):
    return [json.loads(line) for line in path.read_text(encoding='utf-8').splitlines()] if path.exists() else []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    repo = Path(__file__).resolve().parents[2]
    flags = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0
    kernel = ctypes.WinDLL('kernel32', use_last_error=True) if os.name == 'nt' else None
    if kernel:
        kernel.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        kernel.TerminateProcess.argtypes = [ctypes.c_void_p, ctypes.c_uint]
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    with tempfile.TemporaryDirectory(prefix='compatibility-', dir=repo / 'target') as temporary:
        base = Path(temporary)
        fake = base / ('codex.cmd' if os.name == 'nt' else 'codex')
        script = repo / 'tests/fixtures/jsonl_app_server.py'
        fake.write_text(f'@echo off\n"{sys.executable}" -u "{script}" %*\n' if os.name == 'nt'
                        else f'#!/bin/sh\nexec "{sys.executable}" -u "{script}" "$@"\n')
        if os.name != 'nt':
            fake.chmod(0o700)
        for mode in ['version_bad', 'version_empty', 'version_private', 'version_large', 'version_failure',
                     'version_hang', 'initialize_bad', 'thread_version_bad', 'success']:
            root = base / mode
            root.mkdir()
            command = [str(binary), '--cwd', str(repo), '--codex', str(fake), '--journal-dir', str(root / 'journal'),
                       '--run', 'PRIVATE_PROMPT', '--json-events']
            process = subprocess.Popen(command, cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       stdin=subprocess.DEVNULL, creationflags=flags,
                                       env=dict(os.environ, NATIVE_JSONL_FIXTURE_ROOT=str(root), NATIVE_JSONL_FIXTURE_MODE=mode))
            handles = []
            try:
                if mode == 'version_hang' and kernel:
                    deadline = time.monotonic() + 3
                    while not (root / 'pids.json').exists() and process.poll() is None and time.monotonic() < deadline:
                        time.sleep(0.01)
                    for pid in json.loads((root / 'pids.json').read_text()):
                        handle = kernel.OpenProcess(0x00100001, False, pid)
                        assert handle and kernel.WaitForSingleObject(handle, 0) == 258
                        handles.append(handle)
                output, error = process.communicate(timeout=12)
                assert b'PRIVATE_' not in output + error
                stage = [entry['stage'] for entry in values(root / 'startup.jsonl')]
                methods = [entry['method'] for entry in values(root / 'rpc.jsonl')]
                if mode.startswith('version_'):
                    assert process.returncode == 3 and not output, (mode, process.returncode)
                    assert stage == ['version'] and not methods, (mode, stage, methods)
                elif mode == 'initialize_bad':
                    assert process.returncode == 3 and methods == ['initialize'], (mode, process.returncode, methods)
                elif mode == 'thread_version_bad':
                    assert process.returncode == 3 and methods == ['initialize', 'initialized', 'thread/start'], (mode, process.returncode, methods)
                else:
                    assert process.returncode == 0 and methods.count('turn/start') == 1
                if not mode.startswith('version_'):
                    assert stage == ['version', 'catalog', 'app-server'], stage
                assert all(kernel.WaitForSingleObject(handle, 3000) == 0 for handle in handles)
                cursor = next((root / 'journal').glob('*.cursor'))
                session = json.loads(cursor.read_text())['session_id']
                before = (root / 'startup.jsonl').read_bytes()
                replay = subprocess.run([str(binary), '--cwd', str(repo), '--codex', str(base / 'never-execute'),
                                         '--journal-dir', str(root / 'journal'), '--replay', session, '--json-events'],
                                        cwd=repo, capture_output=True, timeout=5, creationflags=flags)
                assert replay.returncode == 0 and b'PRIVATE_' not in replay.stdout + replay.stderr
                assert (root / 'startup.jsonl').read_bytes() == before
                records = [json.loads(line) for line in replay.stdout.splitlines()]
                snapshots = [entry['payload'] for entry in records if entry['kind'] == 'snapshot']
                final = snapshots[-1]
                assert final['session_closed'] and final['cleanup_confirmed'] is True
                assert final['root_start_requests'] == (1 if mode == 'success' else 0)
                for path in (root / 'journal').glob('*'):
                    if path.is_file():
                        assert b'PRIVATE_' not in path.read_bytes()
                print(f'CLI compatibility {mode}: passed (dispatch boundary, redaction, durable replay, query cleanup)')
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                for handle in handles:
                    if kernel.WaitForSingleObject(handle, 0) == 258:
                        kernel.TerminateProcess(handle, 87)
                        kernel.WaitForSingleObject(handle, 3000)
                    kernel.CloseHandle(handle)
                process.stdout.close()
                process.stderr.close()


if __name__ == '__main__':
    main()
