"""Exercise F4 workflow navigation in the real Windows TUI through ConPTY."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

sys.dont_write_bytecode = True
from timeline_tui_check import Console, c, check, k, w
from windows_process_identity import check_clean


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    root = repo / 'target' / ('workflow-tui-' + uuid.uuid4().hex)
    root.mkdir()
    fake = root / 'codex.cmd'
    fake.write_text(f'@echo off\n"{sys.executable}" -u "{repo / "tests/fixtures/jsonl_app_server.py"}" %*\n')
    os.environ['NATIVE_JSONL_FIXTURE_ROOT'] = str(root)
    os.environ['NATIVE_JSONL_FIXTURE_MODE'] = 'workflow_child'
    console = Console([str(args.binary.resolve()), '--cwd', str(repo), '--codex', str(fake),
                       '--journal-dir', str(root / 'journal'), '--tui', 'PRIVATE_PROMPT'], repo)

    def calls():
        path = root / 'rpc.jsonl'
        if not path.exists():
            return []
        content = path.read_text()
        if content and not content.endswith('\n'):
            content = content.rsplit('\n', 1)[0] if '\n' in content else ''
        return [json.loads(line) for line in content.splitlines() if line]

    try:
        console.wait(lambda screen: 'Running' in screen and 'children: 1' in screen,
                     'held root and confirmed child were not shown')
        console.write('queued dependent task\x13')  # Ctrl+S queues against the active root.
        console.write('keep this draft')
        console.write('\x1bOS')  # F4
        console.wait(lambda screen: 'Workflow' in screen and '#3' in screen,
                     'workflow task rows were not shown')
        console.write('\x1b[B\x1b[B')  # Select queued root task #3 after root and native child.
        console.wait(lambda screen: 'Dependencies (AllRequired)' in screen,
                     'selected task dependency details were not shown')
        assert '#1 root task' in console.read() or '#1 PRIVATE_PROMPT' in console.read()
        console.write('d')
        console.wait(lambda screen: '>#1' in screen, 'dependency navigation did not select task #1')
        console.write('\r')  # The active root task opens only its exact current conversation.
        console.wait(lambda screen: 'root · F3 switch' in screen,
                     'Enter did not open the current root conversation')
        assert 'keep this draft' in console.read(), 'Enter discarded the local draft'

        console.write('\x1bOS\x1b[B')  # F4, then select the registered child task.
        console.wait(lambda screen: 'Workflow' in screen and '>#2' in screen,
                     'native child task was not selected')
        console.write('\r')
        console.wait(lambda screen: 'CHILD_OUTPUT' in screen,
                     'Enter did not open the exact confirmed child turn')
        assert 'keep this draft' in console.read(), 'child navigation discarded the local draft'
        console.write('\x1bOR')  # F3 changes the selected Agent locally.
        console.wait(lambda screen: 'root · F3 switch' in screen,
                     'F3 did not switch the local conversation selection')

        before = calls()
        assert sum(row['method'] == 'turn/start' for row in before) == 1
        assert not any(row['method'] == 'turn/interrupt' for row in before)
        console.resize(48, 14)
        console.write('\x1bOS\x1b[F')  # F4, then End to reach workflow relationship details.
        console.wait(lambda screen: 'Workflow' in screen and 'Conversation:' in screen,
                     'workflow relationship details were not reachable at narrow size')
        after = calls()
        execution = ('turn/start', 'turn/interrupt', 'command/exec')
        assert [row for row in after if row['method'] in execution] == [
            row for row in before if row['method'] in execution
        ], 'workflow navigation emitted an additional execution RPC'

        console.write('\x11')  # Ctrl+Q closes the real UI and journal cleanly.
        assert k.WaitForSingleObject(console.process.process, 10000) == 0
        code = w.DWORD()
        check(k.GetExitCodeProcess(console.process.process, c.byref(code)))
        assert code.value == 0
        cursor_files = list((root / 'journal').glob('*.cursor'))
        assert len(cursor_files) == 1, 'journal cursor was not finalized on clean exit'
        cursor = json.loads(cursor_files[0].read_text(encoding='utf-8'))
        assert cursor['session_closed'] is True, 'journal cursor did not record a closed session'
        replay = subprocess.run([
            str(args.binary.resolve()), '--cwd', str(repo), '--codex', str(fake),
            '--journal-dir', str(root / 'journal'), '--export', cursor['session_id'],
        ], cwd=repo, capture_output=True, check=True)
        manifest, _ = json.JSONDecoder().raw_decode(replay.stdout.decode('utf-8'))
        assert manifest['cleanup_confirmed'] is True, 'read-only replay did not confirm child cleanup'
        assert manifest['session_closed'] is True, 'read-only replay did not record a closed session'
        assert manifest['needs_recovery'] is True, (
            'held root/child tasks should remain explicitly recoverable after clean process cleanup'
        )
    except BaseException:
        masked = console.read().replace('PRIVATE_PROMPT', '<REDACTED>')
        masked = re.sub(r'PRIVATE_[A-Z_]+', '<REDACTED>', masked)
        masked = masked.replace('keep this draft', '<DRAFT>')
        (root / 'failure-screen.txt').write_text(masked, encoding='utf-8')
        raise
    finally:
        console.close()
    check_clean(root)
    print('Native workflow TUI passed: dependency navigation, exact root/child Enter, F3 selection, retained draft, narrow details, zero navigation execution RPCs, closed journal, confirmed process cleanup, and explicit recovery for held tasks.')

if __name__ == '__main__':
    main()
