"""Check the shared in-memory projection; no journal or JSONL CLI is implied."""
import json
import sys


def main():
    fixture = json.loads(sys.stdin.buffer.read())
    assert fixture["fixture_version"] == 1
    initial, silence, resumed = fixture["snapshots"]

    def agent(snapshot, name):
        return next(a for a in snapshot["activities"]
                    if a["identity"]["agent_id"] == name and a["scope"] == "turn")

    def tool(snapshot, name):
        return next(a for a in snapshot["activities"] if a["item_id"] == name)

    assert [s["snapshot_version"] for s in fixture["snapshots"]] == [1, 2, 3]
    assert initial["accepted_evidence_count"] == silence["accepted_evidence_count"]
    assert agent(silence, "root")["attention"]["level"] == "attentionNeeded"
    assert agent(resumed, "root")["progress_seq"] == agent(initial, "root")["progress_seq"]
    assert agent(resumed, "a")["attention"]["level"] == "active"
    assert agent(resumed, "b")["attention"]["level"] == "attentionNeeded"
    assert tool(resumed, "tool-one")["attention"]["level"] == "active"
    assert tool(resumed, "tool-two")["attention"]["level"] == "attentionNeeded"
    assert tool(resumed, "tool-two")["progress_seq"] == tool(initial, "tool-two")["progress_seq"]
    parent = agent(resumed, "root")
    assert parent["kind"] == "waitingChildren"
    assert parent["resume_condition"] == "allChildrenCompleteOrAnyUnsuccessful"
    assert [(t["thread_id"], t["turn_id"], t["generation"]) for t in parent["wait_targets"]] == [("a", "a-1", 1), ("b", "b-1", 1)]
    assert [t["silence_ms"] for t in parent["wait_targets"]] == [0, 131000]
    assert all(t["outcome"] is None for t in parent["wait_targets"])
    assert parent["provider_state"] is None
    requests = [a for a in resumed["activities"] if a["scope"] == "interaction"]
    assert {type(a["request_id"]) for a in requests} == {int, str}
    assert all(a["attention"]["requires_action"] for a in requests)
    assert "PRIVATE_" not in json.dumps(fixture)
    print("Python observation consumer: passed (three snapshots, isolated agents/tools, pending requests)")


if __name__ == "__main__":
    main()
