"""A small stdin consumer: EOF is successful only after a durable close snapshot."""
import json
import sys

MAX_LINE_BYTES = 4 * 1024 * 1024
RESULTS = {"completed", "ready", "failed", "interrupted", "unknown"}


def validate(record):
    if not isinstance(record, dict) or type(record.get("schema_version")) is not int or record["schema_version"] != 2:
        raise ValueError("Unsupported schema")
    if record.get("historical") not in (None, False):
        raise ValueError("Historical stream")
    session = record.get("session_id")
    if not isinstance(session, str) or not session or len(session) > 128 or any(ord(c) < 32 for c in session):
        raise ValueError("Invalid session")
    for field in ("event_seq", "snapshot_version"):
        if type(record.get(field)) is not int or record[field] < 0:
            raise ValueError("Invalid sequence")
    payload = record.get("payload")
    if not isinstance(payload, dict) or type(payload.get("session_closed")) is not bool:
        raise ValueError("Invalid state")
    if "cleanup_confirmed" not in payload or payload["cleanup_confirmed"] is not None and type(payload["cleanup_confirmed"]) is not bool:
        raise ValueError("Invalid cleanup")
    if "execution_result" not in payload or payload["execution_result"] is not None and payload["execution_result"] not in RESULTS:
        raise ValueError("Invalid result")
    if payload["session_closed"] and (payload["execution_result"] is None or payload["cleanup_confirmed"] is None):
        raise ValueError("Incomplete close")
    return payload


def main():
    latest = None
    session = None
    sequence = -1
    final = False
    version = -1
    try:
        while line := sys.stdin.buffer.readline(MAX_LINE_BYTES + 1):
            if len(line) > MAX_LINE_BYTES or not line.endswith(b"\n"):
                raise ValueError("Oversized or incomplete record")
            record = json.loads(line)
            payload = validate(record)
            if session is None:
                if record["kind"] != "snapshot" or record["event_seq"] != 0:
                    raise ValueError("Missing initial snapshot")
                session = record["session_id"]
                sequence = 0
            elif final or record["session_id"] != session:
                raise ValueError("Unexpected record after the final snapshot or session changed")
            elif record["kind"] == "state":
                if record["event_seq"] != sequence + 1:
                    raise ValueError("The durable event sequence has a gap")
                sequence += 1
            elif record["kind"] == "snapshot" and record["event_seq"] == sequence:
                if payload != latest or record["snapshot_version"] != version:
                    raise ValueError("Final snapshot differs from committed state")
                final = True
            else:
                raise ValueError("Unexpected stream record")
            if record["snapshot_version"] < version:
                raise ValueError("Snapshot version decreased")
            version = record["snapshot_version"]
            latest = payload
    except (ValueError, KeyError, TypeError, UnicodeError, OSError, RecursionError):
        print("Live stream is malformed or unsupported; inspect the session journal.", file=sys.stderr)
        return 2
    complete = final and latest and latest["session_closed"] and latest["cleanup_confirmed"] is True
    result = latest.get("execution_result") if latest else None
    summary = {"session_id": session, "event_seq": sequence, "stream_closed": bool(complete),
               "execution_result": result, "needs_review": not complete or result in (None, "unknown")}
    print(json.dumps(summary))
    if not complete:
        return 4
    return {"completed": 0, "ready": 0, "failed": 1, "interrupted": 130}.get(result, 4)


if __name__ == "__main__":
    sys.exit(main())
