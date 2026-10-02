"""Validate a durable JSONL replay of the parent/child observation fixture."""
import json
import sys


def main():
    text = sys.stdin.read()
    records = [json.loads(line) for line in text.splitlines()]
    assert "PRIVATE_" not in text
    assert all(r["schema_version"] == 1 and r["historical"] for r in records)
    assert len({r["session_id"] for r in records}) == 1
    assert [r["kind"] for r in records] == [
        "snapshot", "state", "state", "state", "snapshot", "replay_end"]
    assert [r["event_seq"] for r in records] == [0, 1, 2, 3, 3, 3]
    assert [r["snapshot_version"] for r in records] == [1, 2, 3, 4, 4, 4]
    assert records[0]["attempt_id"] == {"task": 1, "attempt": 1}
    initial, silence, resumed = [r["payload"]["observation"] for r in records[:3]]

    def agent(snapshot, name):
        return next(a for a in snapshot["activities"]
                    if a["identity"]["agent_id"] == name and a["scope"] == "turn")

    def tool(snapshot, name):
        return next(a for a in snapshot["activities"] if a["item_id"] == name)

    assert initial["accepted_evidence_count"] == silence["accepted_evidence_count"]
    parent = agent(resumed, "root")
    assert parent["kind"] == "waitingChildren"
    assert parent["resume_condition"] == "allChildrenCompleteOrAnyUnsuccessful"
    assert parent["attention"]["level"] == "attentionNeeded"
    assert parent["progress_seq"] == agent(initial, "root")["progress_seq"]
    assert [(t["thread_id"], t["turn_id"], t["generation"])
            for t in parent["wait_targets"]] == [("a", "a-1", 1), ("b", "b-1", 1)]
    assert [t["silence_ms"] for t in parent["wait_targets"]] == [0, 131000]
    assert all(t["outcome"] is None for t in parent["wait_targets"])
    assert agent(resumed, "a")["attention"]["level"] == "active"
    assert agent(resumed, "b")["attention"]["level"] == "attentionNeeded"
    assert tool(resumed, "tool-one")["attention"]["level"] == "active"
    assert tool(resumed, "tool-two")["attention"]["level"] == "attentionNeeded"
    assert tool(resumed, "tool-two")["progress_seq"] == tool(initial, "tool-two")["progress_seq"]
    for record in records[:-1]:
        assert all(a["freshness"] != "current" for a in record["payload"]["observation"]["activities"])
    requests = [a for a in resumed["activities"] if a["scope"] == "interaction"]
    assert {type(a["request_id"]) for a in requests} == {int, str}
    assert all(a["attention"]["requires_action"] for a in requests)
    latest, end = records[-2]["payload"], records[-1]["payload"]
    assert latest["session_closed"] and latest["cleanup_confirmed"]
    assert latest["execution_result"] == "unknown"
    assert end["high_watermark"] == 3 and not end["live_attached"]
    assert end["needs_recovery"] and end["execution_result"] == "unknown"
    assert end["session_closed"] and not end["uncommitted_tail"]
    print("Python journal consumer: passed (durable cursor, historical evidence, no implied completion)")


if __name__ == "__main__":
    main()
