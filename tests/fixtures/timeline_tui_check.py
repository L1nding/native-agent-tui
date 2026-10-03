"""Exercise the real Windows TUI with a fake server; save only masked evidence screens."""
import argparse
import codecs
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import threading
import time
import unicodedata
import uuid

sys.dont_write_bytecode = True
from windows_terminal_input_check import c, w, k, COORD, SIEX, PI, check
from windows_process_identity import check_clean


class Screen:
    """Small VT screen observer for the cursor/erase sequences used by Crossterm."""
    def __init__(self, width, height):
        self.width, self.height = width, height
        self.rows = [[' '] * width for _ in range(height)]
        self.x = self.y = 0
        self.saved = (0, 0)
        self.state = 'text'
        self.sequence = ''

    def resize(self, width, height):
        self.rows = [(row[:width] + [' '] * width)[:width] for row in self.rows[:height]]
        self.rows += [[' '] * width for _ in range(height - len(self.rows))]
        self.width, self.height = width, height
        self.x, self.y = min(self.x, width - 1), min(self.y, height - 1)

    def newline(self):
        self.y += 1
        if self.y >= self.height:
            self.rows.pop(0)
            self.rows.append([' '] * self.width)
            self.y = self.height - 1

    def csi(self, final):
        if self.sequence.startswith('?'):
            return
        try:
            values = [int(value) if value else 0 for value in self.sequence.split(';')]
        except ValueError:
            return
        n = min((values[0] if values else 0) or 1, max(self.width, self.height))
        if final in 'Hf':
            self.y = min(n - 1, self.height - 1)
            self.x = min(((values[1] if len(values) > 1 else 1) or 1) - 1, self.width - 1)
        elif final == 'G': self.x = min(n - 1, self.width - 1)
        elif final == 'd': self.y = min(n - 1, self.height - 1)
        elif final == 'A': self.y = max(0, self.y - n)
        elif final == 'B': self.y = min(self.height - 1, self.y + n)
        elif final == 'C': self.x = min(self.width - 1, self.x + n)
        elif final == 'D': self.x = max(0, self.x - n)
        elif final == 'X':
            end = min(self.width, self.x + n)
            self.rows[self.y][self.x:end] = [' '] * (end - self.x)
        elif final == 'P':
            row = self.rows[self.y]
            row[self.x:] = (row[self.x + n:] + [' '] * n)[:self.width - self.x]
        elif final == '@':
            row = self.rows[self.y]
            row[self.x:] = ([' '] * n + row[self.x:])[:self.width - self.x]
        elif final == 'K':
            mode = values[0]
            start, end = (0, self.width) if mode == 2 else ((0, self.x + 1) if mode == 1 else (self.x, self.width))
            self.rows[self.y][start:end] = [' '] * (end - start)
        elif final == 'J':
            mode = values[0]
            for row in range(self.height):
                for col in range(self.width):
                    if mode in (2, 3) or (mode == 0 and (row, col) >= (self.y, self.x)) or (mode == 1 and (row, col) <= (self.y, self.x)):
                        self.rows[row][col] = ' '

    def feed(self, text):
        for ch in text:
            if self.state == 'escape':
                if ch == '[': self.state, self.sequence = 'csi', ''
                elif ch == ']': self.state = 'osc'
                else:
                    if ch == '7': self.saved = (self.x, self.y)
                    elif ch == '8': self.x, self.y = self.saved
                    elif ch == 'D': self.newline()
                    elif ch == 'E': self.x = 0; self.newline()
                    elif ch == 'M':
                        if self.y: self.y -= 1
                        else: self.rows.insert(0, [' '] * self.width); self.rows.pop()
                    self.state = 'text'
            elif self.state == 'csi':
                if '@' <= ch <= '~': self.csi(ch); self.state = 'text'
                else:
                    self.sequence += ch
                    if len(self.sequence) > 64: self.state = 'text'
            elif self.state == 'osc':
                if ch == '\x07': self.state = 'text'
                elif ch == '\x1b': self.state = 'osc-escape'
            elif self.state == 'osc-escape':
                self.state = 'text' if ch == '\\' else 'osc'
            elif ch == '\x1b': self.state = 'escape'
            elif ch == '\r': self.x = 0
            elif ch == '\n': self.newline()
            elif ch == '\b': self.x = max(0, self.x - 1)
            elif ch == '\t': self.x = min(self.width - 1, (self.x // 8 + 1) * 8)
            elif ord(ch) >= 32 and not unicodedata.combining(ch):
                width = 2 if unicodedata.east_asian_width(ch) in ('W', 'F') else 1
                if self.x + width > self.width: self.x = 0; self.newline()
                self.rows[self.y][self.x] = ch
                if width == 2: self.rows[self.y][self.x + 1] = ''
                self.x += width

    def text(self):
        return '\n'.join(''.join(row) for row in self.rows)


class Console:
    def __init__(self, command, cwd):
        self.handles = []
        self.hpc = w.HANDLE()
        self.process = PI()
        self.screen = Screen(120, 30)
        self.lock = threading.Lock()
        self.updated = time.monotonic()
        self.attributes = None
        self.initialized = False
        self.reader = None
        try:
            ir, self.iw, ore, ow = (w.HANDLE() for _ in range(4))
            check(k.CreatePipe(c.byref(ir), c.byref(self.iw), None, 0)); self.handles += [ir, self.iw]
            check(k.CreatePipe(c.byref(ore), c.byref(ow), None, 0)); self.handles += [ore, ow]
            if k.CreatePseudoConsole(COORD(120, 30), ir, ow, 0, c.byref(self.hpc)) < 0:
                raise RuntimeError('CreatePseudoConsole failed')
            for handle in (ir, ow): k.CloseHandle(handle); self.handles.remove(handle)
            size = c.c_size_t()
            k.InitializeProcThreadAttributeList(None, 1, 0, c.byref(size))
            self.attributes = c.create_string_buffer(size.value)
            check(k.InitializeProcThreadAttributeList(self.attributes, 1, 0, c.byref(size)))
            self.initialized = True
            check(k.UpdateProcThreadAttribute(self.attributes, 0, 0x20016, self.hpc, c.sizeof(w.HANDLE), None, None))
            startup = SIEX(); startup.si.cb = c.sizeof(SIEX); startup.attributes = c.addressof(self.attributes)
            text = c.create_unicode_buffer(subprocess.list2cmdline(command))
            # The runner may have redirected std handles. Leave them unset during
            # creation so the child receives handles for its attached pseudoconsole.
            k.GetStdHandle.restype = w.HANDLE; k.GetStdHandle.argtypes = [w.DWORD]
            k.SetStdHandle.restype = w.BOOL; k.SetStdHandle.argtypes = [w.DWORD, w.HANDLE]
            standard = [(value & 0xffffffff, k.GetStdHandle(value & 0xffffffff)) for value in (-10, -11, -12)]
            try:
                for name, _ in standard: check(k.SetStdHandle(name, None))
                check(k.CreateProcessW(command[0], text, None, None, False, 0x00080000, None, str(cwd), c.byref(startup), c.byref(self.process)))
            finally:
                for name, handle in standard: check(k.SetStdHandle(name, handle))
            self.handles += [self.process.process, self.process.thread]
            def drain():
                decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
                while True:
                    buffer = c.create_string_buffer(4096); count = w.DWORD()
                    if not k.ReadFile(ore, buffer, len(buffer), c.byref(count), None) or not count.value: break
                    with self.lock:
                        self.screen.feed(decoder.decode(buffer.raw[:count.value]))
                        self.updated = time.monotonic()
            self.reader = threading.Thread(target=drain, daemon=True); self.reader.start()
        except BaseException:
            self.close(); raise

    def write(self, text):
        data = text.encode('utf-8'); sent = w.DWORD()
        check(k.WriteFile(self.iw, data, len(data), c.byref(sent), None))
        assert sent.value == len(data)

    def read(self):
        with self.lock: return self.screen.text()

    def wait(self, predicate, description):
        until = time.monotonic() + 10
        while time.monotonic() < until:
            if predicate(self.read()): return
            if k.WaitForSingleObject(self.process.process, 0) == 0: break
            time.sleep(.025)
        raise AssertionError(description)  # Do not include screen/input contents.

    def stable(self):
        until = time.monotonic() + 2
        while time.monotonic() < until:
            with self.lock:
                if time.monotonic() - self.updated >= .05: return self.screen.text()
            time.sleep(.025)
        raise AssertionError('terminal frame did not settle')

    def resize(self, width, height):
        k.ResizePseudoConsole.restype = c.c_long
        k.ResizePseudoConsole.argtypes = [w.HANDLE, COORD]
        with self.lock:
            self.screen.resize(width, height)
            assert k.ResizePseudoConsole(self.hpc, COORD(width, height)) >= 0

    def close(self):
        if self.process.process and k.WaitForSingleObject(self.process.process, 0) != 0:
            k.TerminateProcess(self.process.process, 1); k.WaitForSingleObject(self.process.process, 3000)
        if self.hpc: k.ClosePseudoConsole(self.hpc); self.hpc = w.HANDLE()
        if self.initialized: k.DeleteProcThreadAttributeList(self.attributes); self.initialized = False
        for handle in self.handles: k.CloseHandle(handle)
        self.handles = []
        if self.reader: self.reader.join(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    root = repo / 'target' / ('timeline-tui-' + uuid.uuid4().hex)
    root.mkdir()
    fake = root / 'codex.cmd'
    fake.write_text(f'@echo off\n"{sys.executable}" -u "{repo / "tests/fixtures/jsonl_app_server.py"}" %*\n')
    os.environ['NATIVE_JSONL_FIXTURE_ROOT'] = str(root)
    os.environ['NATIVE_JSONL_FIXTURE_MODE'] = 'request_details'
    console = Console([str(args.binary.resolve()), '--cwd', str(repo), '--codex', str(fake),
                       '--journal-dir', str(root / 'journal'), '--tui', 'PRIVATE_PROMPT'], repo)
    screenshots = []
    def rpc():
        path = root / 'rpc.jsonl'
        if not path.exists(): return []
        text = path.read_text()
        if text and not text.endswith('\n'): text = text.rsplit('\n', 1)[0] if '\n' in text else ''
        return [json.loads(line) for line in text.splitlines()]
    def answers(): return sum(row['method'] is None for row in rpc())
    try:
        console.wait(lambda screen: 'action:1' in screen, 'initial approval not shown')
        console.write('NEVER_JOURNAL draft')
        # Each navigation selects the latest current pending delivery, including ID reuse.
        for expected in (1, 2, 3):
            before = answers()
            console.write('\x14\x1b[19~' if expected == 1 else '\x14')  # Ctrl+T; F8 once.
            console.wait(lambda screen: 'Evidence timeline' in screen, 'timeline not opened')
            console.write('\x1b[F')  # Explicitly select the latest pending delivery.
            if expected == 1:
                console.write('bB')
                console.wait(lambda screen: '1 bookmarks' in screen, 'bookmark was not saved')
                screenshots.append(console.stable())
                console.write('B')
                console.write('\t\t\t\t')  # Cycle all confirmed relationship scopes.
                console.resize(30, 10)
                console.wait(lambda screen: 'timeline' in screen and 'action:1' in screen, 'narrow live header missing')
                console.resize(120, 30)
                console.write('\x1bOP\x1b[6~\x1bOP')  # Help, scroll, return.
            console.write('\x19\x0e\x02\x13')  # Approval and submit controls have no execution in timeline.
            time.sleep(.1)
            assert answers() == before
            console.write('\r')
            console.wait(lambda screen: 'Context source: server request' in screen, 'current request was not located')
            console.write('\x0e')  # Explicitly decline in the request panel.
            console.wait(lambda _: answers() == expected, 'explicit approval answer missing')
            # Force synchronization with the resulting new delivery / input form.
            if expected < 3:
                console.wait(lambda screen: f'Requests: {3 - expected}' in screen, 'new pending deliveries not projected')
        console.write('\x1bOQ\x1b')  # F2 selects the new input request, then close its details.
        console.wait(lambda screen: 'secret answer' in screen or 'Answer' in screen, 'secret input form missing')
        console.write('\x1b[200~秘密回答中文👋\x1b[201~')
        console.write('\x14')
        console.wait(lambda screen: 'Evidence timeline' in screen, 'timeline not opened over secret form')
        screen = console.stable()
        assert '秘密回答' not in screen and 'NEVER_JOURNAL' not in screen
        screenshots.append(screen)
        console.write('/\x1b[200~input-request\x1b[201~\r')
        console.write('\x14\x1b[200~turn-1\x1b[201~\r')
        assert answers() == 3
        console.write('\x1b[24~')  # F12 closes timeline before showing history.
        console.wait(lambda screen: 'HISTORY' in screen, 'history overlay missing')
        console.write('\x1b[24~\x13')  # F12 returns even while history loads; explicitly submit.
        console.wait(lambda _: (root / 'answers.json').exists(), 'secret answer not submitted')
        assert json.loads((root / 'answers.json').read_text()) == {'valid_fixture_answer': True}
        console.write('\x11')
        assert k.WaitForSingleObject(console.process.process, 10000) == 0
        code = w.DWORD(); check(k.GetExitCodeProcess(console.process.process, c.byref(code))); assert code.value == 0
    except BaseException:
        masked = console.read().replace('NEVER_JOURNAL', '<REDACTED>').replace('秘密回答中文👋', '<REDACTED>')
        masked = re.sub(r'PRIVATE_[A-Z_]+', '<REDACTED>', masked)
        (root / 'failure-screen.txt').write_text(masked, encoding='utf-8')
        raise
    finally:
        console.close()
        check_clean(root)
    calls = rpc()
    assert sum(row['method'] == 'turn/start' for row in calls) == 1
    assert sum(row['method'] == 'turn/interrupt' for row in calls) == 0
    assert sum(row['id'] == 7 and row['method'] is None for row in calls) == 2
    assert sum(row['id'] == 'input-request' and row['method'] is None for row in calls) == 1
    cursor = json.loads(next((root / 'journal').glob('*.cursor')).read_text())
    replay = subprocess.run([str(args.binary.resolve()), '--cwd', str(repo), '--codex', str(root / 'never-execute'),
        '--journal-dir', str(root / 'journal'), '--replay', cursor['session_id'], '--json-events'],
        capture_output=True, timeout=10, check=True, creationflags=subprocess.CREATE_NO_WINDOW)
    for marker in ('PRIVATE_PROMPT', 'NEVER_JOURNAL', '秘密回答'):
        assert marker not in replay.stdout.decode('utf-8')
    assert b'"timeline"' not in replay.stdout
    (root / 'evidence-screens.txt').write_text('\n\n'.join(screenshots), encoding='utf-8')
    (root / 'validation.json').write_text(json.dumps({'root_starts': 1, 'interrupts': 0,
        'approval_answers': 3, 'secret_answer_valid': True, 'cleanup_confirmed': True}), encoding='utf-8')
    print(f'Native timeline TUI passed: scopes/filters/bookmark/help/resize/request ID reuse/secret draft/history, zero observation RPC; {root}')


if __name__ == '__main__': main()
