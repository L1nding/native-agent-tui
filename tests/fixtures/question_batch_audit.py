"""Independently audit captured real 29-item TUI question/answer screens.

Only reads existing screenshots and reports. Negative checks mutate memory only.
This proves a synthetic interaction channel, never project human confirmation.
"""
import argparse
import copy
import json
import re
from pathlib import Path


def require(value, message):
    if not value:
        raise ValueError(message)


def read_evidence(root):
    return {
        "input": json.loads((root / "batch-input.json").read_bytes()),
        "report": json.loads((root / "report.json").read_bytes()),
        "startup": (root / "batch-startup.txt").read_text(encoding="utf-8"),
        "first": (root / "batch-question-first.txt").read_text(encoding="utf-8"),
        "details": (root / "batch-question-details.txt").read_text(encoding="utf-8"),
        "before": [(root / f"batch-answer-{i:02}-before.txt").read_text(encoding="utf-8") for i in range(1, 30)],
        "typed": [(root / f"batch-answer-{i:02}-typed.txt").read_text(encoding="utf-8") for i in range(1, 30)],
        "reply": (root / "batch-reply.txt").read_text(encoding="utf-8"),
    }


def verify(data):
    # The authored synthetic answers are reconstructed independently from captures.
    ids = [f"q{i:02}" for i in range(1, 30)]
    answers = {key: ("Retain" if i % 3 == 1 else f"adjust{i:02}" if i % 3 == 2 else "Defer")
               for i, key in enumerate(ids, 1)}
    require(data["input"]["model"] == "hi/gpt-6.1-sol", "wrong real model")
    require(data["input"]["scope"] == "synthetic user questions only; no domain approval", "scope changed to project approval")
    require(data["input"]["items"] == ids and data["input"]["test_answers"] == answers, "authored input differs")
    require(len(data["report"]) == 5 and all(step["status"] == "PASS" for step in data["report"]), "real run incomplete or failed")
    require("hi/gpt-6.1-sol" in data["startup"] and "Ready" in data["startup"], "actual startup model/ready evidence missing")
    require("(1/29)" in data["first"] and "Item01" in data["first"] and "Enter answer" in data["first"], "first batch question missing")
    detail_ids = re.findall(r"\bid=(q\d{2})\b", data["details"])
    require(list(dict.fromkeys(detail_ids)) == ids, "question IDs missing, reordered or changed")
    require("Requests: 1 | Running" in data["details"] and "Requests: 2" not in data["details"], "not one visible request")
    require(len(data["before"]) == len(data["typed"]) == 29, "keyboard evidence incomplete")
    for i, (before, typed) in enumerate(zip(data["before"], data["typed"]), 1):
        for screen in (before, typed):
            require(f"({i}/29)" in screen and f"Item{i:02}" in screen and "Enter answer" in screen,
                    f"wrong or already submitted question at {i}")
        key = "1" if i % 3 == 1 else f"adjust{i:02}" if i % 3 == 2 else "2"
        editor_lines = typed.splitlines()[-5:]
        require(any(re.search(r"│[·●•] " + re.escape(key) + r"\s+│", line) for line in editor_lines),
                f"actual keyboard answer differs at {i}")
    compact = re.sub(r"\s+", "", data["reply"])
    returned = re.findall(r"(q\d{2}):(Retain|Defer|adjust\d{2})", compact)
    require(returned == list(answers.items()), "Agent lost, duplicated or changed an individual answer")
    require("BATCHACK" in data["reply"] and "Enter answer" not in data["reply"] and "● Running" not in data["reply"], "batch reply not terminal")
    return {"valid": True, "model": data["input"]["model"], "items": 29,
            "scope": "real platform synthetic batch interaction; no project approval",
            "complete_individual_answers": True, "last_custom_answer": "adjust29",
            "project_human_confirmation": "not_run"}


def negative_checks(data):
    mutations = {
        "missing-last-item": lambda d: d.__setitem__("details", d["details"].replace("id=q29", "id=missing")),
        "changed-item-id": lambda d: d.__setitem__("details", d["details"].replace("id=q14", "id=q99")),
        "early-submission": lambda d: d["before"].__setitem__(27, d["before"][27].replace("Enter answer", "Enter send")),
        "wrong-last-keyboard-answer": lambda d: d["typed"].__setitem__(28, d["typed"][28].replace("adjust29", "adjust28")),
        "wrong-last-returned-answer": lambda d: d.__setitem__("reply", d["reply"].replace("q29:adjust29", "q29:adjust28")),
        "failed-run-relabeled": lambda d: d["report"][1].__setitem__("status", "FAIL"),
        "synthetic-answers-as-human-confirmation": lambda d: d["input"].__setitem__("scope", "project approval"),
    }
    rejected = []
    for name, mutate in mutations.items():
        forged = copy.deepcopy(data)
        mutate(forged)
        try:
            verify(forged)
        except ValueError:
            rejected.append(name)
        else:
            raise ValueError("forged batch evidence accepted: " + name)
    return rejected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--negative-tests", action="store_true")
    args = parser.parse_args()
    data = read_evidence(args.evidence)
    result = verify(data)
    if args.negative_tests:
        result["rejected_forgeries"] = negative_checks(data)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
