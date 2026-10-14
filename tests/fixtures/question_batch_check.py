"""Keyboard-only acceptance of one real Agent question containing 29 test items.

Synthetic answers exercise the platform channel, not project geometry approval.
No domain tool, scene mutation or authorization is requested by this scenario.
"""
import json
import re


def expected_answers():
    return {f"q{i:02}": ("Retain" if i % 3 == 1 else f"adjust{i:02}" if i % 3 == 2 else "Defer")
            for i in range(1, 30)}


def run_batch_questions(report, tui, args):
    from dsh_tui_check import CTRL_U, ENTER, ESC, expect
    evidence = report.evidence
    expected = expected_answers()
    (evidence / "batch-input.json").write_text(json.dumps({
        "model": args.model, "profile": args.profile, "items": list(expected),
        "test_answers": expected, "scope": "synthetic user questions only; no domain approval",
    }, indent=2), encoding="utf-8")

    def startup():
        tui.see("Write a task", 120)
        tui.see("Ready", 120)
        tui.see(args.model, 30)
        (evidence / "batch-startup.txt").write_text(tui.screen(), encoding="utf-8")
    if not report.step("batch question startup uses the requested model", startup, tui):
        return

    def ask_once():
        prompt = (
            "This is a synthetic UI test, not a modeling approval. Use only the ask_user_question tool. "
            "Call it exactly ONCE with a questions array containing exactly 29 questions, "
            "with IDs q01 through q29 in that order. Headers must be Item01 through Item29. "
            "Each question is 'Review test item NN', where NN is the two-digit item number, "
            "with two single-choice options labelled Retain and Defer, with short descriptions. "
            "Do not split the questions into separate tool calls, do not use other tools. "
            "Wait for my answers. After the tool returns, output every original question ID and its exact "
            "answer as q01:VALUE;q02:VALUE;...;q29:VALUE, with no summaries or omissions, "
            "then the letters B,A,T,C,H,A,C,K joined without spaces. Do not output this final marker before answers."
        )
        tui.idle()
        tui.keys(CTRL_U, 0.1)
        tui.type(prompt)
        tui.keys(ENTER)
        tui.wait("the real Agent batch question", lambda s: "Enter answer" in s or "Turn failed" in s, 300)
        screen = tui.see("Enter answer", 5)
        expect("(1/29)" in screen and "Item01" in screen, "Agent did not ask one ordered 29-item request")
        (evidence / "batch-question-first.txt").write_text(screen, encoding="utf-8")
        tui.keys("\x1bOQ", 0.3)  # F2 request details
        captured = []
        ids = set()
        for index in range(40):
            screen = tui.screen()
            (evidence / f"batch-details-{index:02}.txt").write_text(screen, encoding="utf-8")
            captured.append(screen)
            ids.update(re.findall(r"\bid=(q\d{2})\b", screen))
            if ids == set(expected):
                break
            tui.keys("\x1b[6~" * 8, 0.15)  # Eight rows; request PageDown scrolls one row.
            if tui.screen() == screen:
                break
        expect(ids == set(expected), f"request detail IDs differ: {sorted(ids)}")
        (evidence / "batch-question-details.txt").write_text("\n\n".join(captured), encoding="utf-8")
        tui.keys(ESC, 0.2)
        expect("(1/29)" in tui.screen(), "browsing details changed the answer position")
    if not report.step("Agent presents all 29 stable item IDs in one request", ask_once, tui):
        return

    def answer_once():
        for i in range(1, 30):
            screen = tui.screen()
            expect(f"({i}/29)" in screen and f"Item{i:02}" in screen, f"wrong answer position at {i}")
            (evidence / f"batch-answer-{i:02}-before.txt").write_text(screen, encoding="utf-8")
            typed = "1" if i % 3 == 1 else f"adjust{i:02}" if i % 3 == 2 else "2"
            tui.type(typed)
            (evidence / f"batch-answer-{i:02}-typed.txt").write_text(tui.screen(), encoding="utf-8")
            tui.keys(ENTER, 0.15)
            if i < 29:
                expect("Enter answer" in tui.screen(), f"batch submitted before item29, at {i}")
        screen = tui.wait("complete individual answers returned by the Agent",
                          lambda s: "BATCHACK" in s and "Enter answer" not in s and "● Running" not in s,
                          300)
        compact = re.sub(r"\s+", "", screen)
        for key, value in expected.items():
            expect(f"{key}:{value}" in compact, f"Agent lost individual answer {key}:{value}")
        (evidence / "batch-reply.txt").write_text(screen, encoding="utf-8")
    if not report.step("all 29 distinct keyboard answers return together without early submission", answer_once, tui):
        return
    report.step("batch question TUI exits successfully", lambda: expect(tui.quit() == 0, "nonzero exit"), tui)
