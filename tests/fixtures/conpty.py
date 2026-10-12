"""A Windows pseudoconsole that runs one program and keeps a readable copy of its screen.

Used by terminal end-to-end checks: write keys, wait for text, read the screen.
"""
import codecs
import subprocess
import sys
import threading
import time
import unicodedata

sys.dont_write_bytecode = True
from windows_terminal_input_check import c, w, k, COORD, SIEX, PI, check

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
