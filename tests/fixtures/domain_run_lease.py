"""OS-held lease for one keyboard acceptance owner per isolated Scene IR."""

import hashlib
import os
from pathlib import Path


def acquire(directory, scene):
    key = hashlib.sha256(
        os.path.normcase(str(Path(scene).resolve())).encode()
    ).hexdigest()
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    lease = (directory / (key + ".lock")).open("a+b")
    if lease.seek(0, os.SEEK_END) == 0:
        lease.write(b"0")
        lease.flush()
    lease.seek(0)
    try:
        if os.name == "nt":
            import msvcrt

            msvcrt.locking(lease.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(lease.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        lease.close()
        raise RuntimeError(
            "another acceptance run still owns this isolated scene"
        ) from None
    return lease
