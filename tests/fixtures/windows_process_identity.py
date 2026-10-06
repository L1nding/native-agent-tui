"""Check fixture process cleanup by PID and creation time, allowing PID reuse."""
import ctypes
import json
import os


def kernel_api():
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel.OpenProcess.restype = ctypes.c_void_p
    kernel.GetProcessTimes.argtypes = [ctypes.c_void_p] + [ctypes.POINTER(ctypes.c_ulonglong)] * 4
    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    return kernel


def creation_time(kernel, handle):
    times = [ctypes.c_ulonglong() for _ in range(4)]
    assert kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in times)), "process identity unavailable"
    return times[0].value


def record(root, pids):
    path = root / "pids.json"
    previous = json.loads(path.read_text()) if path.exists() else []
    # 原子替换：检查方一看到文件存在就会读取，不能让它读到空文件。
    temporary = path.with_name(f"pids.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(previous + pids))
    os.replace(temporary, path)
    if os.name != "nt":
        return
    kernel = kernel_api()
    with (root / "processes.jsonl").open("a") as output:
        for pid in pids:
            handle = kernel.OpenProcess(0x00101000, False, pid)
            assert handle, "fixture process identity unavailable"
            try:
                output.write(json.dumps({"pid": pid, "created": creation_time(kernel, handle)}) + "\n")
            finally:
                kernel.CloseHandle(handle)


def check_clean(root):
    path = root / "processes.jsonl"
    if os.name != "nt":
        return
    pids = root / "pids.json"
    if not pids.exists():
        return
    assert path.exists(), "fixture process identities missing"
    processes = [json.loads(line) for line in path.read_text().splitlines()]
    assert [process["pid"] for process in processes] == json.loads(pids.read_text()), "incomplete fixture process identities"
    kernel = kernel_api()
    reused = 0
    for process in processes:
        handle = kernel.OpenProcess(0x00101000, False, process["pid"])
        if not handle:
            assert ctypes.get_last_error() == 87, "could not verify fixture process cleanup"
            continue
        try:
            if creation_time(kernel, handle) == process["created"]:
                assert kernel.WaitForSingleObject(handle, 3000) == 0, "owned fixture process still alive"
            else:
                reused += 1
        finally:
            kernel.CloseHandle(handle)
    if reused:
        print(f"Fixture process cleanup: {reused} PID identities reused; original processes exited")
