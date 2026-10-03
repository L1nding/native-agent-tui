"""Native Windows input checks; fixed UTF-8 fixtures, no captured input text."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import uuid

k = c.WinDLL('kernel32', use_last_error=True)
class COORD(c.Structure):
    _fields_ = [('X', c.c_short), ('Y', c.c_short)]
class SI(c.Structure):
    _fields_ = [('cb', w.DWORD), ('reserved', w.LPWSTR), ('desktop', w.LPWSTR), ('title', w.LPWSTR), ('x', w.DWORD), ('y', w.DWORD), ('xsize', w.DWORD), ('ysize', w.DWORD), ('xchars', w.DWORD), ('ychars', w.DWORD), ('fill', w.DWORD), ('flags', w.DWORD), ('show', w.WORD), ('reserved2size', w.WORD), ('reserved2', c.c_void_p), ('stdin', w.HANDLE), ('stdout', w.HANDLE), ('stderr', w.HANDLE)]
class SIEX(c.Structure):
    _fields_ = [('si', SI), ('attributes', c.c_void_p)]
class PI(c.Structure):
    _fields_ = [('process', w.HANDLE), ('thread', w.HANDLE), ('pid', w.DWORD), ('tid', w.DWORD)]
for name, restype, args in [
    ('CreatePipe', w.BOOL, [c.POINTER(w.HANDLE), c.POINTER(w.HANDLE), c.c_void_p, w.DWORD]),
    ('CreatePseudoConsole', c.c_long, [COORD, w.HANDLE, w.HANDLE, w.DWORD, c.POINTER(w.HANDLE)]),
    ('ClosePseudoConsole', None, [w.HANDLE]),
    ('InitializeProcThreadAttributeList', w.BOOL, [c.c_void_p, w.DWORD, w.DWORD, c.POINTER(c.c_size_t)]),
    ('UpdateProcThreadAttribute', w.BOOL, [c.c_void_p, w.DWORD, c.c_size_t, c.c_void_p, c.c_size_t, c.c_void_p, c.c_void_p]),
    ('DeleteProcThreadAttributeList', None, [c.c_void_p]),
    ('CreateProcessW', w.BOOL, [w.LPCWSTR, w.LPWSTR, c.c_void_p, c.c_void_p, w.BOOL, w.DWORD, c.c_void_p, w.LPCWSTR, c.POINTER(SIEX), c.POINTER(PI)]),
    ('ReadFile', w.BOOL, [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]),
    ('WriteFile', w.BOOL, [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]),
    ('WaitForSingleObject', w.DWORD, [w.HANDLE, w.DWORD]),
    ('TerminateProcess', w.BOOL, [w.HANDLE, w.UINT]),
    ('GetExitCodeProcess', w.BOOL, [w.HANDLE, c.POINTER(w.DWORD)]),
    ('CloseHandle', w.BOOL, [w.HANDLE]),
]:
    fn = getattr(k, name); fn.restype = restype; fn.argtypes = args

def check(ok):
    if not ok:
        raise c.WinError(c.get_last_error())

def run_case(executable, root, record, chunks):
    os.environ['NATIVE_TERMINAL_FIXTURE_RECORD'] = str(record)
    handles = []
    hpc = w.HANDLE(); attributes = None; initialized = False
    process = PI(); reader = None
    try:
        ir, iw, ore, ow = (w.HANDLE() for _ in range(4))
        check(k.CreatePipe(c.byref(ir), c.byref(iw), None, 0)); handles += [ir, iw]
        check(k.CreatePipe(c.byref(ore), c.byref(ow), None, 0)); handles += [ore, ow]
        hr = k.CreatePseudoConsole(COORD(120,30), ir, ow, 0, c.byref(hpc))
        if hr < 0: raise RuntimeError('CreatePseudoConsole failed')
        k.CloseHandle(ir); k.CloseHandle(ow)
        handles.remove(ir); handles.remove(ow)
        size = c.c_size_t()
        k.InitializeProcThreadAttributeList(None, 1, 0, c.byref(size))
        attributes = c.create_string_buffer(size.value)
        check(k.InitializeProcThreadAttributeList(attributes, 1, 0, c.byref(size)))
        initialized = True
        check(k.UpdateProcThreadAttribute(attributes, 0, 0x20016, hpc, c.sizeof(w.HANDLE), None, None))
        startup = SIEX(); startup.si.cb = c.sizeof(SIEX); startup.attributes = c.addressof(attributes)
        command = c.create_unicode_buffer(subprocess.list2cmdline([str(executable)]))
        check(k.CreateProcessW(str(executable), command, None, None, False, 0x00080000, None, str(root), c.byref(startup), c.byref(process)))
        handles += [process.process, process.thread]
        ready = threading.Event()
        def drain():
            received = bytearray()
            while True:
                buffer = c.create_string_buffer(4096); n = w.DWORD()
                if not k.ReadFile(ore, buffer, len(buffer), c.byref(n), None) or n.value == 0: break
                received.extend(buffer.raw[:n.value])
                if b'INPUT_FIXTURE_READY' in received: ready.set()
                if len(received) > 65536: del received[:-32768]
        reader = threading.Thread(target=drain, daemon=True); reader.start()
        if not ready.wait(5): raise RuntimeError('input fixture not ready')
        for frame, pause in chunks:
            sent = w.DWORD(); check(k.WriteFile(iw, frame, len(frame), c.byref(sent), None))
            assert sent.value == len(frame)
            if pause: time.sleep(pause)
        if k.WaitForSingleObject(process.process, 7000) != 0: raise RuntimeError('input fixture timeout')
        code = w.DWORD(); check(k.GetExitCodeProcess(process.process, c.byref(code))); assert code.value == 0
        return json.loads(record.read_text('utf-8'))
    finally:
        if process.process and k.WaitForSingleObject(process.process, 0) != 0:
            k.TerminateProcess(process.process, 1); k.WaitForSingleObject(process.process, 3000)
        if hpc: k.ClosePseudoConsole(hpc)
        if initialized: k.DeleteProcThreadAttributeList(attributes)
        for handle in handles: k.CloseHandle(handle)
        if reader: reader.join(timeout=2)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--split-markers', action='store_true', help='Probe host support for splitting unknown CSI markers across writes; some ConPTY hosts drop their prefix')
    parser.add_argument('--enter-modifiers', action='store_true', help='Require the ConPTY host to retain Shift/Ctrl+Enter in encoded Win32 records')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    output = root / 'target' / ('native-input-' + uuid.uuid4().hex)
    output.mkdir()
    def frame(body):
        return [(("\x1b[200~" + body + "\x1b[201~\x11").encode('utf-8'), 0)]
    keys = "\x06\x1bOP\x1bOQ\x1bOR\x1bOS\x1b[15~\x1b[17~\x1b[18~\x1b[19~\x1b[20~\x1b[21~\x1b[23~\x1b[24~\x1b[1;5H\x1b[1;5F\x1b[A\x1b[B\x1b[D\x1b[C\x1b[5~\x1b[6~\x1b[H\x1b[F\x1b[3~\x7f\t\x1b[Z\x1b[27;2;13~\x1b[13;5u\x11"
    physical = "\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_\x1b[13;28;13;1;8;1_\x1b[13;28;13;0;8;1_\x1b[113;0;0;1;0;1_\x1b[113;0;0;0;0;1_\x11"
    cases = [
        ('lf', frame('FIRST中文👋\nSECOND'), 'exact_paste', 1, 1, 0, 0, True),
        ('crlf', frame('FIRST中文👋\r\nSECOND'), 'exact_paste', 1, 1, 0, 0, True),
        ('cr', frame('FIRST中文👋\rSECOND'), 'exact_paste', 1, 1, 0, 0, True),
        ('controls', frame('FIRST中文👋\nSECOND\x03\x19\x0e\x11\x1bOP'), 'control_paste', 1, 1, 0, 0, True),
        ('limit', frame('中' * (32768 // 3) + 'ab'), 'limit_paste', 1, 1, 0, 0, True),
        ('overflow', frame('a' * 32769 + '\n\x03\x19\x0e\x11'), None, 0, 1, 0, 1, True),
        ('fragmented-body', [('\x1b[200~FIRST中文👋'.encode('utf-8'), .12), (b'\nSECOND', .12), (b'\x1b[201~\x11', 0)], 'exact_paste', 1, 1, 0, 0, True),
        ('incomplete', [('\x1b[200~secret中文👋\n\x11\x03\x19'.encode('utf-8'), 0)], None, 0, 0, 0, 0, False),
        ('keys', [(keys.encode('utf-8'), 0)], 'keys_match', 0, 30, 2, 0, True),
        ('physical-keys', [(physical.encode('utf-8'), 0)], 'physical_keys_match' if args.enter_modifiers else 'physical_records_decoded', 0, 4, 2, 0, True),
        ('portable-keys', [(b'\x0f\x13\x11', 0)], 'portable_keys_match', 0, 3, 0, 0, True),
        ('unknown-csi', [(b'\x1b[?9999h\x11', 0)], None, 0, 1, 0, 0, True),
    ]
    if args.split_markers:
        cases.append(('split-markers', [(b'\x1b[20', .12), ('0~FIRST中文👋\nSECOND\x1b[20'.encode('utf-8'), .12), (b'1~\x11', 0)], 'exact_paste', 1, 1, 0, 0, True))
    for name, chunks, verdict, pastes, key_count, enters, rejected, quit_ in cases:
        report = run_case(args.fixture.resolve(), root, output / (name + '.json'), chunks)
        assert report['mode_restored'], f'{name}: console mode not restored'
        assert report['paste_events'] == pastes, f'{name}: paste count {report}'
        assert report['key_events'] == key_count, f'{name}: unexpected key event {report}'
        assert report['enter_events'] == enters, f'{name}: unexpected submit key {report}'
        assert report['rejected_events'] == rejected, f'{name}: incorrect limit handling {report}'
        assert report['quit'] == quit_, f'{name}: incorrect control handling {report}'
        if verdict: assert report[verdict], f'{name}: fixture changed {report}'
        print(f'Native input {name}: passed', flush=True)
    print(f'Windows terminal input verified: {output}', flush=True)

if __name__ == '__main__': main()
