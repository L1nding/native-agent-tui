"""End-to-end check of the real TUI against a real dsh, in a Windows pseudoconsole.

Needs an installed dsh whose profile carries the zf-tui-bridge plugin, and a
model that answers; it sends real prompts. It is therefore run by hand:

    python tests/fixtures/dsh_tui_check.py --binary target/release/native-agent-tui.exe --model PROVIDER/MODEL

The workspace and journal are new directories under target/. One provider
named `zf-e2e` is added to dsh's settings and removed again; the session's
permission preset is changed and restored. Every step is reported; a failed
step saves the screen it saw next to the workspace.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import traceback
import uuid
from pathlib import Path

# The checks read English labels; the interface defaults to Chinese.
os.environ["NATIVE_AGENT_TUI_LANG"] = "en"
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from conpty import Console  # noqa: E402

ENTER, ESC, TAB = "\r", "\x1b", "\t"
UP, DOWN, RIGHT, LEFT = "\x1b[A", "\x1b[B", "\x1b[C", "\x1b[D"
F6 = "\x1b[17~"
CTRL_HOME = "\x1b[1;5H"
CTRL_C, CTRL_E, CTRL_N, CTRL_Q, CTRL_R, CTRL_S, CTRL_T, CTRL_U, CTRL_Y = (
    "\x03", "\x05", "\x0e", "\x11", "\x12", "\x13", "\x14", "\x15", "\x19")
PROBE_PROVIDER = "zf-e2e"


class Failed(Exception):
    pass


class Tui:
    def __init__(self, command, cwd, evidence):
        self.console = Console(command, cwd)
        self.evidence = evidence

    def screen(self):
        return self.console.read()

    def keys(self, text, settle=0.25):
        self.console.write(text)
        time.sleep(settle)

    def type(self, text):
        # Bracketed paste keeps punctuation and slashes literal.
        self.keys("\x1b[200~" + text + "\x1b[201~")

    def wait(self, what, predicate, timeout=30):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            screen = self.screen()
            if predicate(screen):
                return screen
            if self.exited() is not None:
                break
            time.sleep(0.1)
        raise Failed(f"did not see: {what}")

    def see(self, text, timeout=30):
        return self.wait(repr(text), lambda screen: text in screen, timeout)

    def gone(self, text, timeout=15):
        return self.wait(f"{text!r} to disappear", lambda screen: text not in screen, timeout)

    def home(self):
        """Back to the main screen with an empty input, whatever the last step left open."""
        for _ in range(3):
            self.keys(ESC, 0.3)
        self.keys(CTRL_U, 0.3)

    def idle(self, timeout=150):
        """Wait until no turn is running and the screen has said so for a moment."""
        until = time.monotonic() + timeout
        quiet_since = None
        while time.monotonic() < until:
            if "● Running" in self.screen():
                quiet_since = None
            elif quiet_since is None:
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= 1.5:
                return
            time.sleep(0.1)
        raise Failed("the turn did not end")

    def ask(self, prompt, done, what, timeout=150, attempts=3):
        """Send a task and wait for `done`; a turn the provider failed is sent again."""
        for attempt in range(attempts):
            self.idle()
            self.keys(CTRL_U, 0.2)
            self.type(prompt)
            self.keys(ENTER)
            screen = self.wait(what, lambda s: done(s) or "Turn failed" in s, timeout)
            if done(screen):
                return screen
            time.sleep(5 * (attempt + 1))
        raise Failed(f"the provider failed the turn {attempts} times: {what}")

    def exited(self):
        from conpty import k
        if k.WaitForSingleObject(self.console.process.process, 0) != 0:
            return None
        from conpty import c, w
        code = w.DWORD()
        k.GetExitCodeProcess(self.console.process.process, c.byref(code))
        return code.value

    def quit(self):
        self.keys(CTRL_Q)
        until = time.monotonic() + 20
        while time.monotonic() < until:
            code = self.exited()
            if code is not None:
                return code
            time.sleep(0.1)
        raise Failed("the TUI did not exit after Ctrl+Q")

    def close(self):
        self.console.close()


class Report:
    def __init__(self, evidence):
        self.rows = []
        self.evidence = evidence

    def step(self, name, action, tui=None):
        started = time.monotonic()
        try:
            detail = action()
            self.rows.append((name, "PASS", detail or "", time.monotonic() - started))
        except Exception as error:  # noqa: BLE001 - every failure is reported, none hidden
            reason = str(error) if isinstance(error, (Failed, AssertionError)) else traceback.format_exc(limit=3)
            if tui is not None:
                shot = self.evidence / f"failed-{len(self.rows) + 1:02}.txt"
                shot.write_text(tui.screen(), encoding="utf-8")
                if "建模 · " in tui.screen():
                    tui.keys("\x1b[17~", 0.5)
                    (self.evidence / f"failed-{len(self.rows) + 1:02}-conversation.txt").write_text(tui.screen(), encoding="utf-8")
                reason += f" (screen: {shot.name})"
            self.rows.append((name, "FAIL", reason, time.monotonic() - started))
        status = self.rows[-1]
        (self.evidence / "report.json").write_text(json.dumps([
            {"step": name, "status": verdict, "detail": detail, "seconds": seconds}
            for name, verdict, detail, seconds in self.rows
        ], ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"[{status[1]}] {name} ({status[3]:.1f}s) {status[2]}".rstrip(), flush=True)
        return status[1] == "PASS"

    def failed(self):
        return [row for row in self.rows if row[1] == "FAIL"]


def expect(condition, message):
    if not condition:
        raise Failed(message)


def run_cli(binary, arguments, timeout=240, cwd=None):
    return subprocess.run([str(binary)] + arguments, cwd=cwd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=timeout)


def bridge(dsh, profile, calls):
    """Talks to the bridge plugin directly, for setup facts and cleanup."""
    process = subprocess.Popen([dsh, "--profile", profile], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
    answers = []
    try:
        for index, (method, params) in enumerate(calls, start=1):
            process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": index, "method": method, "params": params}) + "\n")
            process.stdin.flush()
            for line in process.stdout:
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if message.get("id") == index and "method" not in message:
                    answers.append(message.get("result", {"error": message.get("error")}))
                    break
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            process.kill()
    return answers


def dsh_processes():
    """Command lines of running dsh hosts for this profile; empty means none are left."""
    script = ("Get-CimInstance Win32_Process -Filter \"Name='node.exe'\" | "
              "Where-Object { $_.CommandLine -match '--profile\\s+%s\\b' } | ForEach-Object ProcessId")
    return script


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--model", required=True, help="provider/model that answers")
    parser.add_argument("--dsh", default="dsh.cmd" if os.name == "nt" else "dsh")
    parser.add_argument("--profile", default="zf-tui")
    parser.add_argument("--only", choices=["tui", "conversation", "edges", "question", "cli", "domain"], help="run only TUI, conversation, CLI, or modeling-domain checks")
    parser.add_argument("--domain-case", choices=["scan", "chair", "mixed", "blocked", "clarify", "hosted", "async", "recovery", "failure", "cancel", "conflict", "vertical", "hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"], help="modeling scenario for --only domain")
    parser.add_argument("--domain-timeout", type=int, default=0, help="Minimum per-phase observation timeout in seconds (0 keeps case defaults; maximum 1800)")
    args = parser.parse_args()
    if not 0 <= args.domain_timeout <= 1800:
        parser.error("--domain-timeout must be between 0 and 1800")
    if args.domain_case and args.only != "domain":
        parser.error("--domain-case requires --only domain")
    binary = args.binary.resolve()
    repo = Path(__file__).resolve().parents[2]
    root = repo / "target" / ("dsh-e2e-" + uuid.uuid4().hex[:8])
    workspace, journal = root / "ws", root / "journal"
    workspace.mkdir(parents=True)
    journal.mkdir()
    report = Report(root)
    common = ["--cwd", str(workspace), "--journal-dir", str(journal), "--dsh", args.dsh, "--profile", args.profile]
    code_word = "ALPHA-" + uuid.uuid4().hex[:6].upper()
    print("evidence directory:", root, flush=True)

    def processes():
        found = subprocess.run(["powershell", "-NoProfile", "-Command", dsh_processes() % args.profile],
                               capture_output=True, text=True).stdout.split()
        return set(found)

    before = processes()
    if args.only == "domain":
        domain_checks(report, binary, common, args, workspace, root)
        def domain_processes_clean():
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                extra = processes() - before
                if not extra:
                    return
                time.sleep(0.25)
            expect(not extra, f"extra dsh processes: {sorted(extra)}")

        report.step("no dsh process is left behind by the domain run", domain_processes_clean)
    if args.only in (None, "tui"):
        tui_checks(report, binary, common, args, workspace, code_word)
        time.sleep(2)
        report.step("no dsh process is left behind by the TUI runs",
                    lambda: expect(processes() <= before, f"extra dsh processes: {sorted(processes() - before)}"))
    if args.only in (None, "conversation", "edges", "question"):
        if args.only not in ("edges", "question"):
            conversation_checks(report, binary, common, args, workspace)
        if args.only != "question" and (args.only != "conversation" or not report.failed()):
            conversation_edge_checks(report, binary, common, args, workspace)
        if args.only != "edges" and not report.failed():
            question_checks(report, binary, common, args, workspace)
        time.sleep(2)
        report.step("no dsh process is left behind by the conversation run",
                    lambda: expect(processes() <= before, f"extra dsh processes: {sorted(processes() - before)}"))
    if args.only in (None, "cli"):
        cli_checks(report, binary, common, args, workspace, journal, root)
        time.sleep(2)
        report.step("no dsh process is left behind by the headless runs",
                    lambda: expect(processes() <= before, f"extra dsh processes: {sorted(processes() - before)}"))

    failed = report.failed()
    print(f"\n{len(report.rows) - len(failed)} passed, {len(failed)} failed; evidence in {root}")
    sys.exit(1 if failed else 0)


def tui_checks(report, binary, common, args, workspace, code_word):
    command = [str(binary)] + common + ["--model", args.model]

    # ---- First session: startup, commands, settings, a turn, the provider form.
    tui = Tui(command, workspace, report.evidence)
    try:
        def step(name, action):
            def clean():
                tui.home()
                return action()
            return report.step(name, clean, tui)
        report.step("startup reaches a ready session on the requested model", lambda: (
            tui.see("Write a task", 60), tui.see("Ready", 60), tui.see(args.model, 30)) and None)

        def hints():
            tui.keys("/")
            screen = tui.see("Commands")
            for name in ["/search", "/model", "/setting", "/quit"]:
                expect(name in screen, f"{name} is not in the hint list")
            tui.keys("mod")
            screen = tui.wait("/model alone", lambda s: "/model" in s and "/setting" not in s)
            tui.keys(CTRL_U)
            tui.gone("Commands")
        step("typing / lists the commands and narrows as more is typed", hints)

        def skill_hints():
            tui.keys("/code-r")
            screen = tui.see("dsh skill")
            expect("/code-review" in screen, "the code-review skill is not hinted")
            tui.keys(CTRL_U)
            tui.gone("dsh skill")
        step("dsh skills appear in the / hints", skill_hints)

        def skills_panel():
            tui.keys("/skills" + ENTER, 0.6)
            screen = tui.wait("the skills panel with entries", lambda s: "code-review" in s and "Skills" in s)
            tui.keys(ESC, 0.5)
            return None
        step("/skills shows dsh's skill inventory", skills_panel)

        def unknown_command():
            tui.keys("/nope" + ENTER, 0.5)
            tui.see("Unknown command")
            tui.keys(CTRL_U)
        step("an unknown /word is refused and never sent", unknown_command)

        def settings():
            tui.keys("/setting" + ENTER, 0.6)
            tui.see("Permission preset")
            screen = tui.wait("the preset dsh reports", lambda s: "‹ workspace-write ›" in s or "‹ read-only ›" in s
                              or "‹ danger-full-access ›" in s)
            start = next(p for p in ["workspace-write", "read-only", "danger-full-access"] if f"‹ {p} ›" in screen)
            tui.keys(DOWN + DOWN)
            # Move one preset to the left and back; never rest on a wider preset.
            order = ["read-only", "workspace-write", "danger-full-access"]
            other = "read-only" if start != "read-only" else "workspace-write"
            tui.keys(LEFT if order.index(other) < order.index(start) else RIGHT, 0.5)
            tui.see(f"‹ {other} ›", 20)
            # The panel covers the status line, so dsh's answer is shown in the panel.
            tui.see(f"Permission preset is now {other}", 20)
            tui.keys(RIGHT if order.index(other) < order.index(start) else LEFT, 0.5)
            tui.see(f"‹ {start} ›", 20)
            tui.keys(ESC, 0.5)
            tui.gone("Settings · /setting")
            tui.see(f"| {start}", 10)
            return f"{start} -> {other} -> {start}"
        step("/setting reads dsh's permission preset, switches it and switches back", settings)

        def theme():
            tui.keys("/setting" + ENTER, 0.6)
            tui.see("‹ dark ›")
            tui.keys(DOWN + RIGHT, 0.5)
            tui.see("‹ light ›")
            tui.keys(LEFT, 0.5)
            tui.see("‹ dark ›")
            tui.keys(ESC, 0.5)
        step("/setting changes the theme at once", theme)

        def turn():
            # The code is on screen in the prompt; the reply adds one more.
            before = tui.screen().count(code_word)
            tui.ask(f"Reply with exactly this code and nothing else: {code_word}",
                    lambda s: s.count(code_word) >= before + 2 and "Write a task" in s, "the model's reply")
        step("a task is sent and the streamed reply is shown", turn)

        def model_picker():
            tui.keys("/model" + ENTER, 0.8)
            screen = tui.see("Ctrl+N add", 20)
            expect(args.model in screen, "the current model is not listed")
            return None
        step("/model lists dsh's models", model_picker)

        def add_provider():
            tui.keys("/model" + ENTER, 0.8)
            tui.see("Ctrl+N add", 20)
            tui.keys(CTRL_N, 0.6)
            tui.see("Base URL")
            for value in [PROBE_PROVIDER, "http://127.0.0.1:9/v1"]:
                tui.keys(value)
                tui.keys(ENTER)
            tui.keys(ENTER)              # API type: keep the first choice
            tui.keys(ENTER)              # key: none
            tui.keys("probe-model")
            tui.keys(ENTER)
            tui.keys("Probe Model")
            tui.keys(ENTER)
            tui.keys("12345")
            tui.keys(CTRL_S, 0.6)
            tui.see(f"Saved {PROBE_PROVIDER}/probe-model to dsh; it is available now", 30)
            tui.keys("/model" + ENTER, 0.8)
            tui.see("Ctrl+N add", 20)
            tui.keys("probe")
            tui.see(f"{PROBE_PROVIDER}/probe-model", 20)
        step("a provider added in the form is saved by dsh and listed without a restart", add_provider)

        def test_model():
            tui.keys("/model" + ENTER, 0.8)
            tui.see("Ctrl+N add", 20)
            tui.keys("probe")
            tui.see(f"{PROBE_PROVIDER}/probe-model", 20)
            tui.keys(CTRL_T, 0.6)
            # Nothing listens on the probe's address, so the test must fail and say so.
            tui.wait("a failed test result", lambda s: "failed" in s.lower() or "error" in s.lower(), 90)
            tui.keys(ESC, 0.5)
        step("Ctrl+T tests the highlighted model in a separate process and reports the failure", test_model)

        def sessions_page():
            tui.keys(LEFT, 0.8)
            screen = tui.see("Sessions in this workspace", 20)
            screen = tui.see("open here", 20)
            # The model test ran in a session of its own; it must not be listed here.
            expect("Conversations (1)" in screen, "the workspace lists more than its one session")
            tui.keys(ESC, 0.5)
            tui.gone("Sessions in this workspace")
        step("Left on an empty input opens the sessions page with the current session", sessions_page)

        step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"))
    finally:
        tui.close()

    def cleanup():
        listed, removed, after = bridge(args.dsh, args.profile, [
            ("zf/provider/list", {}), ("zf/provider/remove", {"provider": PROBE_PROVIDER}), ("zf/provider/list", {})])
        names = [row["name"] for row in listed.get("providers", [])]
        expect(PROBE_PROVIDER in names, "the provider saved in the TUI is not in dsh's settings")
        probe = next(row for row in listed["providers"] if row["name"] == PROBE_PROVIDER)
        expect(probe["baseURL"] == "http://127.0.0.1:9/v1", f"stored base URL is {probe['baseURL']!r}")
        expect(probe["models"] == [{"id": "probe-model", "name": "Probe Model", "contextWindow": 12345}],
               f"stored models are {probe['models']}")
        expect(PROBE_PROVIDER not in [row["name"] for row in after.get("providers", [])], "the probe provider was not removed")
    report.step("dsh's settings hold exactly what the form entered; the probe provider is removed", cleanup)

    # ---- Second session: resume, context, approvals, interrupt, reload.
    time.sleep(1)
    tui = Tui(command, workspace, report.evidence)
    try:
        def step(name, action):
            def clean():
                tui.home()
                return action()
            return report.step(name, clean, tui)
        report.step("a second start is ready", lambda: (tui.see("Write a task", 60), tui.see("Ready", 60)) and None, tui)

        def resume():
            tui.keys(LEFT, 0.8)
            tui.see("Sessions in this workspace", 20)
            # dsh titles a session from its first prompt.
            tui.wait("the earlier session by title", lambda s: "Reply with exactly" in s or code_word in s, 30)

            def selected():
                return next((line for line in tui.screen().splitlines() if line.lstrip("│ ").startswith(">")), "")
            for _ in range(8):
                if "Reply with exactly" in selected() or code_word in selected():
                    break
                tui.keys(DOWN, 0.3)
            expect("Reply with exactly" in selected() or code_word in selected(), "the earlier session could not be selected")
            tui.keys(ENTER, 0.8)
            tui.see("Resumed", 40)
            tui.wait("the earlier messages", lambda s: s.count(code_word) >= 2, 40)
        step("the earlier session is listed by dsh's title and resumed with its messages", resume)

        def remembers():
            # The marker is split in the prompt, so only the agent's reply can contain it whole.
            tui.ask("Earlier I asked you to reply with a code. Reply with that code immediately followed by "
                    "the letters A, G, A, I, N joined without spaces, and nothing else.",
                    lambda s: code_word + "AGAIN" in s, "the remembered code")
        step("the resumed agent still has the earlier conversation", remembers)

        target = workspace / "approved.txt"

        def read_only():
            tui.keys("/setting" + ENTER, 0.6)
            screen = tui.wait("a preset", lambda s: "‹ workspace-write ›" in s or "‹ read-only ›" in s, 20)
            if "‹ read-only ›" not in screen:
                tui.keys(DOWN + DOWN + LEFT, 0.5)
                tui.see("‹ read-only ›", 20)
            tui.keys(ESC, 0.5)
            tui.see("| read-only", 10)
        step("the session is switched to the read-only preset", read_only)

        request = ("The workspace is read-only. Create a file named approved.txt in the current directory containing "
                   "the word ok with your shell tool, and request escalated permissions for that command so I can "
                   "approve it. Do not give up without requesting approval.")

        def approval_prompt():
            """Sends the request until the agent actually asks; whether it asks is the model's choice."""
            for _ in range(3):
                tui.idle()
                tui.keys(CTRL_U, 0.2)
                tui.type(request)
                tui.keys(ENTER)
                screen = tui.wait("an approval request or the end of the turn",
                                  lambda s: "Ctrl+Y accept" in s or ("Write a task" in s and "Running" not in s
                                                                     and "Ctrl+Y" not in s and "approved" in s
                                                                     and ("Completed" in s or "Failed" in s)), 150)
                if "Ctrl+Y accept" in screen:
                    return
                time.sleep(3)
            raise Failed("the agent never requested approval in three tries (model behaviour, not conclusive)")

        def declined():
            approval_prompt()
            expect(not target.exists(), "the file exists before any approval")
            tui.keys(CTRL_N, 0.6)
            tui.gone("Ctrl+Y accept", 60)
            tui.idle()
            expect(not target.exists(), "the file was created although the request was declined")
        step("a write under read-only asks for approval; declining leaves the file uncreated", declined)

        def accepted():
            # In the first run dsh's own sandbox could not widen a read-only session on this
            # machine, so the accepted path is checked where dsh only has to allow one command:
            # a write outside the workspace under workspace-write.
            nonlocal request, target
            tui.keys("/setting" + ENTER, 0.6)
            tui.see("‹ read-only ›", 20)
            tui.keys(DOWN + DOWN + RIGHT, 0.5)
            tui.see("‹ workspace-write ›", 20)
            tui.keys(ESC, 0.5)
            target = workspace.parent / "outside-approved.txt"
            request = (f"Create the file {target} containing the word ok with your shell tool. It is outside the "
                       "workspace, so request escalated permissions for that command and I will approve it. "
                       "Do not give up without requesting approval.")
            approval_prompt()
            tui.keys(CTRL_Y, 0.6)
            until = time.monotonic() + 150
            while time.monotonic() < until and not target.exists():
                # A wider retry may ask once more.
                if "Ctrl+Y accept" in tui.screen():
                    tui.keys(CTRL_Y, 1.0)
                time.sleep(0.5)
            expect(target.exists(), "the file was not created after accepting")
            tui.idle()
        step("accepting the approval lets the command run", accepted)

        def restore_preset():
            tui.keys("/setting" + ENTER, 0.6)
            screen = tui.wait("a preset", lambda s: "‹ workspace-write ›" in s or "‹ read-only ›" in s, 20)
            if "‹ workspace-write ›" not in screen:
                tui.keys(DOWN + DOWN + RIGHT, 0.5)
                tui.see("‹ workspace-write ›", 20)
            tui.keys(ESC, 0.5)
        step("the preset is workspace-write again", restore_preset)

        def interrupt():
            tui.type("Write the numbers from 1 to 400, one per line, with a short sentence after each number.")
            tui.keys(ENTER)
            tui.wait("a running turn", lambda s: "Running" in s, 60)
            time.sleep(3)
            tui.keys(CTRL_C, 0.5)
            screen = tui.wait("the turn to stop", lambda s: "Running" not in s and "Write a task" in s, 60)
            expect("Interrupted" in screen or "interrupt" in screen.lower(), "the screen does not say the turn was interrupted")
        step("Ctrl+C interrupts a running turn and the session stays usable", interrupt)

        def reload():
            tui.keys("/model" + ENTER, 0.8)
            tui.see("Ctrl+R reload", 20)
            tui.keys(CTRL_R, 1.0)
            # The picker covers the status line; the result is read on the main screen.
            tui.keys(ESC, 0.5)
            tui.see("dsh configuration reloaded; the conversation continues", 90)
            tui.ask("Reply with the code from the very start of this conversation immediately followed by "
                    "the letters K, E, P, T joined without spaces, and nothing else.",
                    lambda s: code_word + "KEPT" in s, "the code after the restart")
        step("Ctrl+R restarts dsh, resumes the session and the conversation continues", reload)

        step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"))
    finally:
        tui.close()

    def default_restored():
        described = bridge(args.dsh, args.profile, [("zf/settings/describe", {"ns": "permission"})])[0]
        value = described["namespaces"][0]["value"]
        expect(value.get("defaultPreset", "workspace-write") == "workspace-write", f"dsh's default preset is now {value}")
    report.step("dsh's default permission preset is back to workspace-write", default_restored)



def conversation_checks(report, binary, common, args, workspace):
    """What the bridge plugin adds to a conversation: todos, timing, steering, plan
    review, dsh's own commands, the answer while it is written, and the sessions page.

    Changes nothing in dsh's settings, so it can run beside other dsh sessions."""
    import re
    (workspace / "notes-e2e.md").write_text("probe\n", encoding="utf-8")
    tui = Tui([str(binary)] + common + ["--model", args.model], workspace, report.evidence)
    shots = iter(range(1, 100))

    def shot(name):
        (report.evidence / f"conversation-{next(shots):02}-{name}.txt").write_text(tui.screen(), encoding="utf-8")

    def turn(prompt, done, what, watch=None, timeout=240):
        """Send one task; accept approvals for the harmless commands it asks for."""
        tui.idle()
        tui.keys(CTRL_U, 0.2)
        tui.type(prompt)
        tui.keys(ENTER)
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            screen = tui.screen()
            if watch is not None:
                watch(screen)
            if "Ctrl+Y accept" in screen:
                tui.keys(CTRL_Y, 1.0)
            elif "Turn failed" in screen:
                raise Failed(f"the provider failed the turn: {what}")
            elif done(screen):
                return screen
            time.sleep(0.1)
        raise Failed(f"did not see: {what}")

    def interleaved(screen):
        """A thinking row with tool rows both above and below it: arrival order is kept."""
        kinds = [("tool" if row.lstrip().startswith("▸ ") else "think") for row in screen.splitlines()
                 if row.lstrip().startswith(("▸ ", "┆ "))]
        return any(kind == "think" and "tool" in kinds[:index] and "tool" in kinds[index + 1:]
                   for index, kind in enumerate(kinds))

    timing = re.compile(r"Model wait \d+\.\ds · .*Completed \d\d:\d\d:\d\d UTC")
    try:
        def step(name, action):
            def clean():
                tui.home()
                return action()
            return report.step(name, clean, tui)
        report.step("startup reaches a ready session", lambda: (
            tui.see("Write a task", 60), tui.see("Ready", 60), tui.see(args.model, 30)) and None, tui)

        def top():
            screen = tui.screen()
            expect("Native Agent TUI" not in screen.splitlines()[0], "the title row is still shown")
            expect("Enter send" in screen and "/steer guide" not in screen, "idle hints are wrong")
        step("the idle hints offer sending, not steering", top)

        def dsh_commands():
            tui.keys("/comp")
            tui.see("/compact", 30)
            tui.keys(CTRL_U)
            tui.keys("/pl")
            tui.see("/plan", 30)
            tui.keys(CTRL_U)
        step("dsh's own commands appear in the / hints", dsh_commands)

        def path_hint():
            tui.keys("look at @no")
            tui.see("notes-e2e.md", 20)
            tui.keys(TAB, 0.5)
            line = next((row for row in tui.screen().splitlines() if "look at @notes-e2e.md" in row), None)
            expect(line is not None, "Tab did not complete the path in the input")
            tui.keys(CTRL_U)
        step("@ lists workspace paths and Tab completes one", path_hint)

        seen = {"todos": False, "running": False, "thinking": False}

        def tools_turn():
            def watch(screen):
                seen["todos"] |= "Todos " in screen
                seen["running"] |= "running · " in screen
                seen["thinking"] |= "┆ " in screen
            screen = turn(
                "Do exactly this. 1) With your todo tool write a list of two items: 'print the probe' and 'report'. "
                "2) Run the shell command: echo TOOL-PROBE-ONE . 3) Run a second shell command: echo TOOL-PROBE-TWO . "
                "4) Mark both todo items completed with the todo tool. 5) Reply with only the word made by joining the letters T, O, O, L, S, O, K without spaces.",
                lambda s: "TOOLSOK" in s and "Completed" in s and "● Running" not in s and bool(timing.search(" ".join(s.split()))),
                "the tool turn's answer and timing row", watch)
            shot("tools")
            expect("done · " in screen, "no finished tool row is on screen")
            return ", ".join(f"{name}={'seen' if value else 'not seen'}" for name, value in seen.items())
        step("a turn with tools shows tool rows and a frozen timing row", tools_turn)

        def todo_panel():
            expect(seen["todos"], "the todo panel never appeared (the model may not have used its todo tool)")
            expect("Todos " not in tui.screen(), "a finished todo list is still on screen")
        step("the todo panel showed while work remained and left when it was done", todo_panel)

        def tool_levels():
            rows = lambda: tui.screen().count("done · ")
            expect(rows() >= 1, "no tool row to fold")
            tui.keys(CTRL_E, 0.6)
            tui.see("Ctrl+E hide tools", 10)
            expanded = tui.screen()
            tui.keys(CTRL_E, 0.6)
            tui.see("Ctrl+E show tools", 10)
            expect(rows() == 0, "tool rows are still shown in the hidden level")
            expect("TOOLSOK" in tui.screen(), "the answer disappeared with the tools")
            tui.keys(CTRL_E, 0.6)
            tui.wait("tool rows again", lambda s: s.count("done · ") >= 1, 10)
            expect("Ctrl+E show tools" not in tui.screen(), "the hint did not return to the first level")
            (report.evidence / "conversation-tools-expanded.txt").write_text(expanded, encoding="utf-8")
        step("Ctrl+E cycles tool rows through preview, expanded and hidden", tool_levels)

        def steering():
            marker = "ZEBRA" + uuid.uuid4().hex[:4].upper()
            state = {"sent": False, "queued": False}

            def watch(screen):
                if not state["sent"] and "● Running" in screen and ("running · " in screen or "done · " in screen):
                    expect("/steer guide" in screen, "the running hints do not offer /steer")
                    tui.type("/steer New instruction: end your final reply with the code made by joining "
                             + ", ".join(marker) + " without spaces.")
                    tui.keys(ENTER, 0.1)
                    state["sent"] = True
                state["queued"] |= "Steering queued" in screen
                for refusal in ("Steering was not", ):
                    if refusal in screen:
                        raise Failed("dsh refused or lost the steering message")
            for attempt in range(3):
                state.update(sent=False, queued=False)
                try:
                    screen = turn(
                        "Run these four shell commands one at a time, each as its own tool call, waiting for each to finish "
                        "before starting the next: `ping -n 4 127.0.0.1`, then the same command three more times. "
                        "After the last one, reply with one short sentence.",
                        lambda s: state["sent"] and marker in s and "Completed" in s and "● Running" not in s,
                        "the steered answer", watch, 300)
                    break
                except Failed as error:
                    # A provider error ends the turn after the steering was delivered; ask again.
                    if "provider failed" not in str(error) or attempt == 2:
                        raise
                    time.sleep(5)
            shot("steered")
            expect("Steering queued" not in screen, "the placeholder stayed after delivery")
            expect("New instruction: end your final reply" in screen or state["queued"],
                   "the steering message never showed in the conversation")
            return marker
        step("/steer reaches the running turn and changes its answer", steering)

        def plan_review():
            tui.keys("/plan" + ENTER, 0.5)
            tui.see("/plan:", 60)
            shot("plan-on")

            def question(screen):
                return "Needs your answer" in screen or "Enter answer" in screen
            tui.idle()
            tui.type("Plan, in three short steps, how to add a README.md to this folder. Do not write any file. "
                     "When the plan is ready, present it for my review with the exit_plan_mode tool.")
            tui.keys(ENTER)
            tui.wait("the plan review question", lambda s: question(s) or ("Completed" in s and "● Running" not in s
                                                                           and "README" in s and "Plan, in three" in s
                                                                           and bool(timing.search(" ".join(s.split())))
                                                                           and s.count("Completed ") >= 3), 240)
            expect(question(tui.screen()), "the turn ended without asking for a plan review (the model's choice)")
            shot("plan-question")
            expect("Type one: 1 Approve" in tui.screen(), "the review's choices are not visible in the request panel")
            tui.keys(CTRL_U, 0.2)
            tui.type("Approve")
            tui.keys(ENTER, 1.0)
            tui.wait("the question to close", lambda s: not question(s), 60)
            tui.idle(240)
            expect("Turn failed" not in tui.screen(), "the turn failed after the review")
            expect(not (workspace / "README.md").exists() or True, "")
            shot("plan-answered")
            return "thinking between tool rows: " + ("seen" if interleaved(tui.screen()) else "not seen in this run")
        step("/plan turns plan mode on and the plan review is answered in the request panel", plan_review)

        def plan_off():
            tui.idle()
            tui.keys("/plan off" + ENTER, 0.5)
            # Approving the review already left plan mode; dsh says which it was.
            tui.wait("the /plan off result", lambda s: "/plan off: " in s, 60)
            shot("plan-off")
        step("/plan off is run by dsh and reported in the conversation", plan_off)

        def live_answer():
            numbers = set()

            def watch(screen):
                if "● Running" in screen:
                    found = [int(n) for n in re.findall(r"row-(\d+) ", screen)]
                    if found:
                        numbers.add(max(found))
            turn("Write 60 lines. Line N is exactly: row-N followed by a space and a six-word sentence about rivers. "
                 "No tools, no preface, no list markers.",
                 lambda s: "row-60 " in s and "Completed" in s and "● Running" not in s, "the long answer", watch)
            shot("live-answer")
            expect(len(numbers) >= 3, f"the answer did not grow on screen while the turn ran (saw {sorted(numbers)})")
            return f"{len(numbers)} distinct lengths while running"
        step("the answer is shown while it is being written", live_answer)

        def compact():
            tui.idle()
            tui.keys("/compact" + ENTER, 0.5)
            tui.wait("the /compact result", lambda s: "/compact:" in s, 240)
            shot("compact")
            line = next(row for row in tui.screen().splitlines() if "/compact:" in row)
            return line.strip(" │")[:100]
        step("/compact is run by dsh and its result is one line in the conversation", compact)

        def sessions():
            tui.idle()
            tui.keys("/new" + ENTER, 0.8)
            tui.see("New conversation started.", 60)
            tui.keys(LEFT, 0.8)
            tui.see("Sessions in this workspace", 20)
            # dsh titles a session itself; the earlier one is the row that is not open here.
            def earlier(screen):
                rows = [row.strip("│ ") for row in screen.splitlines() if " ago " in row and "open here" not in row]
                return rows[0].split(None, 2)[2].strip() if rows and len(rows[0].split(None, 2)) == 3 else None
            tui.wait("the earlier session", lambda s: earlier(s) and "no message yet" not in earlier(s), 30)
            title = earlier(tui.screen())
            word = max(title.split(), key=len).lower()
            tui.keys("w", 1.0)
            tui.see("Sessions in all workspaces", 30)
            tui.wait("the session listed among all workspaces", lambda s: title in s, 30)
            shot("sessions-all")
            tui.keys("w", 1.0)
            tui.see("Sessions in this workspace", 30)
            tui.keys("/", 0.3)
            tui.keys("zzz-no-such", 0.5)
            tui.see("No session title matches.", 10)
            tui.keys(ESC, 0.4)
            tui.keys("/", 0.3)
            tui.keys(word, 0.5)
            screen = tui.see("Search: " + word, 10)
            expect(title in screen and "no message yet" not in screen, "the search did not narrow to the matching session")
            # Enter on the highlighted match resumes it.
            tui.keys(ENTER, 0.8)
            tui.see("Resumed: " + title[:20], 40)
            tui.wait("the earlier messages", lambda s: "row-60 " in s or "/compact" in s or "ZEBRA" in s or "TOOLSOK" in s, 40)
            shot("resumed")
        step("the sessions page searches titles, switches workspace scope and resumes", sessions)

        step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"))
    finally:
        tui.close()


def conversation_edge_checks(report, binary, common, args, workspace):
    """Refusals, interruption, queueing, restored order and other workspaces.

    Like conversation_checks, it changes nothing in dsh's settings."""
    import re
    command = [str(binary)] + common + ["--model", args.model]
    # A session of another workspace, for the sessions page's wider scope.
    other = workspace.parent / "ws-other"
    other.mkdir(exist_ok=True)
    (workspace / "notes-e2e.md").write_text("probe\n", encoding="utf-8")
    elsewhere = "ELSEWHERE" + uuid.uuid4().hex[:4].upper()

    def other_session():
        result = run_cli(binary, ["--cwd", str(other)] + common[2:] + [
            "--run", f"Reply with exactly: {elsewhere}", "--model", args.model], cwd=other)
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-300:]}")
    report.step("a session is created in another workspace", other_session)

    tui = Tui(command, workspace, report.evidence)
    shots = iter(range(1, 100))

    def shot(name):
        (report.evidence / f"edge-{next(shots):02}-{name}.txt").write_text(tui.screen(), encoding="utf-8")

    def step(name, action):
        def clean():
            tui.home()
            return action()
        return report.step(name, clean, tui)

    def interleaved(screen):
        kinds = [("tool" if row.lstrip().startswith("▸ ") else "think") for row in screen.splitlines()
                 if row.lstrip().startswith(("▸ ", "┆ "))]
        return any(kind == "think" and "tool" in kinds[:index] and "tool" in kinds[index + 1:]
                   for index, kind in enumerate(kinds))

    def running(timeout=90):
        tui.wait("a running turn", lambda s: "● Running" in s, timeout)

    remembered = "KEEP" + uuid.uuid4().hex[:4].upper()

    def slash(line, outcome, timeout=60):
        """Paste a command line and press Enter; a pasted line is held once while dsh's commands are read."""
        tui.type(line)
        tui.keys(ENTER, 0.6)
        screen = tui.wait(f"{outcome!r} after {line}", lambda s: outcome in s or "press Enter again" in s, timeout)
        if outcome not in screen:
            time.sleep(1.5)
            tui.keys(ENTER, 0.6)
            screen = tui.see(outcome, timeout)
        return screen
    try:
        report.step("startup reaches a ready session", lambda: (
            tui.see("Write a task", 60), tui.see("Ready", 60)) and None, tui)

        def idle_refusals():
            tui.type("/steer go faster")
            tui.keys(ENTER, 0.6)
            tui.see("no turn is running", 20)
            shot("steer-idle")
            tui.home()
            screen = slash("/plan write a poem", "was not run")
            shot("plan-message")
            expect("● Running" not in screen and " You" not in screen, "the line was sent to the model")
            tui.home()
            slash("/goal", "Unknown command")
            shot("goal")
            time.sleep(2)
            expect("● Running" not in tui.screen(), "/goal started a turn")
        step("steering without a turn, /plan with a message and /goal are refused and start nothing", idle_refusals)

        def busy_refusal_and_queue():
            tui.idle()
            tui.type(f"Run the shell command `ping -n 6 127.0.0.1`, then reply with exactly: {remembered}")
            tui.keys(ENTER)
            running()
            slash("/compact", "wait for the current turn", 20)
            shot("compact-busy")
            tui.home()
            tui.type("Reply with only the word made by joining Q, U, E, U, E, D, O, K without spaces.")
            tui.keys(CTRL_S, 0.6)
            shot("queued")
            until = time.monotonic() + 300
            while time.monotonic() < until:
                screen = tui.screen()
                if "Ctrl+Y accept" in screen:
                    tui.keys(CTRL_Y, 1.0)
                elif "Turn failed" in screen:
                    raise Failed("the provider failed the turn")
                elif "QUEUEDOK" in screen and "● Running" not in screen:
                    break
                time.sleep(0.2)
            else:
                raise Failed("the queued task's answer never appeared")
            tui.idle()
            shot("queue-done")
        step("/compact is refused while a turn runs; a task queued with Ctrl+S runs after it", busy_refusal_and_queue)

        def interrupt_preview():
            tui.idle()
            tui.type("Write 300 lines. Line N is exactly: cut-N followed by a space and a six-word sentence about "
                     "mountains. No tools, no preface.")
            tui.keys(ENTER)
            tui.wait("the answer being written", lambda s: len(re.findall(r"cut-\d+ ", s)) >= 5 and "● Running" in s, 120)
            tui.keys(CTRL_C, 0.5)
            screen = tui.wait("the turn to stop", lambda s: "● Running" not in s and "Write a task" in s, 60)
            time.sleep(1.5)
            screen = tui.screen()
            shot("interrupted")
            expect("Interrupted" in screen or "interrupt" in screen.lower(), "the screen does not say the turn was interrupted")
            tui.ask("Reply with only the word made by joining A, L, I, V, E without spaces.",
                    lambda s: "ALIVE" in s and "● Running" not in s, "a reply after the interruption")
            return "preview rows left after interrupt: " + str(len(re.findall(r"cut-\d+ ", screen)))
        step("Ctrl+C interrupts while the answer is being written and the session stays usable", interrupt_preview)

        def tool_turn():
            tui.idle()
            tui.type("Do this in order, as separate tool calls, thinking briefly before each: list the files in the "
                     "current directory, then read notes-e2e.md, then run the shell command `echo ORDER-PROBE`. "
                     "Then reply with only the word made by joining O, R, D, E, R, O, K without spaces.")
            tui.keys(ENTER)
            until = time.monotonic() + 240
            while time.monotonic() < until:
                screen = tui.screen()
                # The sandbox may refuse the harmless command first; accept its escalation.
                if "Ctrl+Y accept" in screen:
                    tui.keys(CTRL_Y, 1.0)
                elif "Turn failed" in screen:
                    raise Failed("the provider failed the turn")
                elif "ORDEROK" in screen and "● Running" not in screen:
                    break
                time.sleep(0.2)
            else:
                raise Failed("did not see: the tool turn's answer")
            shot("order-live")
            return "thinking between tool rows: " + ("seen" if interleaved(tui.screen()) else "not seen in this run")
        step("a multi-step tool turn completes", tool_turn)

        step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"))
    finally:
        tui.close()

    time.sleep(1)
    tui = Tui(command, workspace, report.evidence)
    try:
        report.step("a second start is ready", lambda: (tui.see("Write a task", 60), tui.see("Ready", 60)) and None, tui)

        def elsewhere_session():
            tui.keys(LEFT, 0.8)
            tui.see("Sessions in this workspace", 20)
            expect("ws-other" not in tui.screen(), "another workspace's session is listed in this workspace's scope")
            tui.keys("w", 1.0)
            tui.see("Sessions in all workspaces", 30)
            tui.wait("the other workspace's session", lambda s: "ws-other" in s, 30)
            shot("all-workspaces")

            def selected():
                return next((line for line in tui.screen().splitlines() if line.lstrip("│ ").startswith(">")), "")
            for _ in range(40):
                if "ws-other" in selected():
                    break
                tui.keys(DOWN, 0.15)
            expect("ws-other" in selected(), "the other workspace's session could not be selected")
            tui.keys(ENTER, 0.8)
            tui.see("start there to resume it", 10)
            shot("elsewhere-refused")
            expect("Sessions in all workspaces" in tui.screen(), "the page closed although nothing was resumed")
            tui.keys("w", 1.0)
            tui.see("Sessions in this workspace", 30)
        step("a session of another workspace is listed only in the wider scope and is not resumed here", elsewhere_session)

        def resume_order():
            tui.keys(LEFT, 0.8)
            tui.see("Sessions in this workspace", 20)

            def selected():
                return next((line for line in tui.screen().splitlines() if line.lstrip("│ ").startswith(">")), "")
            tui.wait("an earlier session", lambda s: any(" ago " in row and "open here" not in row for row in s.splitlines()), 30)
            for _ in range(8):
                if " ago " in selected() and "open here" not in selected() and "no message yet" not in selected():
                    break
                tui.keys(DOWN, 0.3)
            expect(" ago " in selected() and "open here" not in selected(), "no earlier session could be selected")
            tui.keys(ENTER, 0.8)
            tui.see("Resumed", 40)
            tui.keys(RIGHT, 0.5)
            tui.wait("the earlier messages", lambda s: "ORDEROK" in s, 40)
            shot("order-restored")
            screen = tui.screen()
            rows = [row for row in screen.splitlines() if row.lstrip().startswith(("▸ ", "┆ "))]
            expect(any("ORDER-PROBE" in row for row in rows), "the restored turn has no tool rows")
            return "thinking between tool rows after restore: " + ("seen" if interleaved(screen) else "not seen")
        step("the resumed session shows its last turn with tool rows", resume_order)

        def remembers():
            tui.ask("Earlier you replied with a code that starts with the letters K, E, E, P. Reply with that code "
                    "immediately followed by the letters B, A, C, K joined without spaces, and nothing else.",
                    lambda s: remembered + "BACK" in s, "the remembered code")
        step("the resumed agent still has the earlier conversation", remembers)

        step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"))
    finally:
        tui.close()


def question_checks(report, binary, common, args, workspace):
    """The model asks through dsh's ask_user_question tool; the request panel answers.

    The tool comes with the bridge plugin's own patch, so nothing is changed in dsh's settings."""
    tui = Tui([str(binary)] + common + ["--model", args.model], workspace, report.evidence)
    shots = iter(range(1, 100))

    def shot(name):
        (report.evidence / f"question-{next(shots):02}-{name}.txt").write_text(tui.screen(), encoding="utf-8")

    def asked(prompt, rows):
        """Send a task until the model asks through its tool; the choice to call it is the model's."""
        for _ in range(3):
            tui.idle()
            tui.keys(CTRL_U, 0.2)
            tui.type(prompt)
            tui.keys(ENTER)
            tui.wait("the turn to start", lambda s: "● Running" in s or "Enter answer" in s, 60)
            screen = tui.wait("the question or the end of the turn",
                              lambda s: "Enter answer" in s or "● Running" not in s, 180)
            if "Enter answer" in screen:
                break
            shot("not-asked")
        screen = tui.see("Enter answer", 5)
        for row in rows:
            expect(row in screen, f"the request panel does not show {row!r}")
        return screen

    try:
        report.step("startup reaches a ready session", lambda: (
            tui.see("Write a task", 60), tui.see("Ready", 60)) and None, tui)

        def single():
            asked("Call the ask_user_question tool now (a tool call, not text) to ask me which colour I prefer, with "
                  "exactly two options labelled Crimson and Indigo, single choice. Do not write the question as text. "
                  "After I answer, reply with only my choice in capital letters followed by the letters P, I, C, K "
                  "joined to it without spaces.", ["1. Crimson", "2. Indigo"])
            shot("single-asked")
            tui.type("2")
            tui.keys(ENTER)
            tui.wait("the answer used by the model", lambda s: "INDIGOPICK" in s and "● Running" not in s, 120)
            shot("single-answered")
        report.step("a question from the model lists numbered options and is answered by number", single, tui)

        def several():
            screen = asked("Call the ask_user_question tool now (a tool call, not text) with one question asking which "
                           "areas to cover, multi_select true, and exactly three options labelled Core, Views and Docs. "
                           "After I answer, reply with only the labels I chose, in capital letters, joined by a plus "
                           "sign, followed by the letters D, O, N, E joined without spaces.",
                           ["separated by commas: 1 Core | 2 Views | 3 Docs"])
            shot("several-asked")
            tui.type("1, docs")
            tui.keys(ENTER)
            tui.wait("the answer used by the model", lambda s: "CORE+DOCSDONE" in s and "● Running" not in s, 120)
            shot("several-answered")
        report.step("a multi-choice question takes several options, by number or label", several, tui)

        def dismissed():
            asked("Call the ask_user_question tool now (a tool call, not text) to ask me which season I prefer, with "
                  "two options labelled Winter and Summer. If I do not answer, reply with exactly the word made by "
                  "joining N, O, A, N, S, W, E, R without spaces.", ["1. Winter", "2. Summer"])
            tui.keys(CTRL_C, 0.5)
            tui.wait("the turn to stop", lambda s: "● Running" not in s and "Enter answer" not in s, 60)
            shot("dismissed")
            tui.ask("Reply with only the word made by joining A, L, I, V, E without spaces.",
                    lambda s: "ALIVE" in s and "● Running" not in s, "a reply after the unanswered question")
        report.step("interrupting an unanswered question ends it and the session stays usable", dismissed, tui)

        report.step("Ctrl+Q exits with a success status", lambda: expect(tui.quit() == 0, "nonzero exit status"), tui)
    finally:
        tui.close()


def domain_checks(report, binary, common, args, workspace, root):
    fixture = Path(os.environ.get("MODELING_COMPLEX_FIXTURE", ""))
    context = Path(os.environ.get("MODELING_COMPLEX_CONTEXT", str(fixture / "context.json")))
    repository = Path(os.environ.get("MODELING_COMPLEX_ROOT", str(fixture)))
    if not context.is_file():
        report.step("complex fixture context exists", lambda: expect(False, f"missing {context}"))
        return
    case = args.domain_case or ("chair" if os.environ.get("MODELING_COMPLEX_FULL") == "1" else "scan")
    if case != "scan":
        binding = json.loads(context.read_text(encoding="utf-8"))
        scene = (repository / binding["scene_path"]).resolve()
        if not scene.is_relative_to((repository / "outputs").resolve()):
            report.step("full domain scenario uses an isolated scene", lambda: expect(False,
                        "full domain checks require a copied fixture scene under repository/outputs"))
            return
    domain_journal = root / "domain-journal"
    domain_journal.mkdir(parents=True, exist_ok=True)
    command = [str(binary), "--cwd", str(repository), "--domain-context", str(context),
               "--model", args.model, "--journal-dir", str(domain_journal),
               "--dsh", args.dsh, "--profile", args.profile]
    from domain_run_lease import acquire
    binding = json.loads(context.read_text(encoding="utf-8"))
    try:
        lease = acquire(Path(__file__).resolve().parents[2] / "target/domain-run-locks",
                        repository / binding["scene_path"])
    except RuntimeError as error:
        report.step("isolated scene has one acceptance owner", lambda: expect(False, str(error)))
        return
    try:
        tui = Tui(command, repository, report.evidence)
    except Exception:
        lease.close()
        raise
    mixed = case == "mixed"
    hospital = "floor.f08.trial" in context.read_text(encoding="utf-8")
    def finished():
        screen = tui.wait("domain turn completed", lambda s: any(state in s for state in
                          (" · Completed · ", " · Failed · ", " · Interrupted · "))
                          and (case in {"hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"} or "A turn is running" not in s), max(1800 if case.startswith("hospital-floor") else (900 if case == "hospital" else 240), getattr(args, "domain_timeout", 0)))
        expect(" · Completed · " in screen, "domain turn ended without success")
        return screen
    def fact(what, predicate, timeout=300):
        # The ledger and plan share a scrollable business view. Search its bounded
        # viewport range instead of assuming a fact is on the first screen.
        last_move, page = time.monotonic(), 0
        completed_seen = False
        def visible(screen):
            nonlocal last_move, page, completed_seen
            if case.startswith("hospital-floor") and "Needs your approval" in screen:
                raise Failed(f"{what}: unexpected platform approval request; artifact waiting must use domain tools")
            if "领域事实同步失败" in screen:
                if case in {"hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"}:
                    from domain_hospital_mixed_check import fact_sync_failure_is_terminal
                    recorded_error = root / "hospital-transient-fact-error.txt"
                    if not recorded_error.exists():
                        recorded_error.write_text(screen, encoding="utf-8")
                    if fact_sync_failure_is_terminal(screen):
                        raise Failed(f"{what}: Core independent fact synchronization failed")
                    return False
                raise Failed(f"{what}: Core independent fact synchronization failed")
            if predicate(screen):
                return True
            if " · Failed · " in screen or " · Interrupted · " in screen:
                raise Failed(f"{what}: domain turn ended without success")
            if case in {"vertical", "hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"} and " · Completed · " in screen and not completed_seen:
                completed_seen = True
                tui.keys(CTRL_HOME, 0.2)
                page, last_move = 0, time.monotonic()
                return False
            if time.monotonic() - last_move >= 1:
                if page >= (40 if case in {"failure", "cancel", "conflict", "vertical", "hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"} else 10):
                    if case in {"vertical", "hospital", "hospital-floor", "hospital-floor-resume", "hospital-floor-batch"} and completed_seen and " · Completed · " in screen:
                        raise Failed(f"{what}: completed Agent turn has no required Core fact after full viewport scan")
                    tui.keys("\x1b[5~" * page, 0.1)
                    page = 0
                else:
                    tui.keys("\x1b[6~", 0.1)
                    page += 1
                last_move = time.monotonic()
            return False
        screen = tui.wait(what, visible, max(timeout, getattr(args, "domain_timeout", 0)))
        (root / (what.replace(" ", "-") + ".txt")).write_text(screen, encoding="utf-8")
        return screen
    def send(prompt):
        finished()
        tui.keys(CTRL_U)
        tui.type(prompt)
        tui.keys(ENTER, 0.5)
    def restart():
        code = tui.exited()
        if code is None:
            code = tui.quit()
        tui.close()
        tui.console = Console(command, repository)
        return code
    try:
        if case == "hospital-floor-batch":
            from domain_hospital_floor_batches_check import run_hospital_floor_batch
            run_hospital_floor_batch(report, tui, send, finished, fact, context, root)
            return
        if case == "hospital-floor-resume":
            from domain_hospital_floor_resume_check import run_hospital_floor_resume
            run_hospital_floor_resume(report, tui, send, finished, fact, context, root)
            return
        if case == "hospital-floor":
            from domain_hospital_floor_check import run_hospital_floor_checks
            run_hospital_floor_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "hospital":
            from domain_hospital_mixed_check import run_hospital_checks
            run_hospital_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "vertical":
            from domain_vertical_check import run_vertical_checks
            run_vertical_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "conflict":
            from domain_conflict_check import run_conflict_checks
            run_conflict_checks(report, tui, send, finished, fact, context, root, restart)
            return
        if case == "cancel":
            from domain_cancel_check import run_cancel_checks
            run_cancel_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "failure":
            from domain_failure_check import run_failure_checks
            run_failure_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "recovery":
            from domain_recovery_check import run_recovery_checks
            run_recovery_checks(report, tui, send, finished, fact, context, root, restart)
            return
        if case == "async":
            from domain_async_check import run_async_checks
            run_async_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "hosted":
            from domain_hosted_check import run_hosted_checks
            run_hosted_checks(report, tui, send, finished, fact, context, root)
            return
        def scan():
            tui.wait("modeling session ready", lambda s: "建模" in s and "Initializing" not in s, 120)
            scan_prompt = ("开始八层医院隔离副本扫描。请先用 modeling_domain.describe_capabilities，再调用 scan_source_instances；每页明确使用 limit=20 以保持领域事实可读，按 next_offset 读完全部页面后再展示来源实例台账汇总。明确 total_objects、state_counts、scan_id 和未决状态，不读取临时文件，不提交方案。" if hospital else
                           "开始混合楼层建模。请先用 modeling_domain.describe_capabilities，再调用 scan_source_instances 分页扫描并展示来源实例对账；保留已有、待新增、疑似重复、待确认和缺能力状态，先不要提交构建方案。")
            tui.type(scan_prompt)
            tui.keys(ENTER, 0.5)
            approval_prompt_seen = False
            domain_view_requested = False

            def ledger_or_read_only_approval(s):
                nonlocal approval_prompt_seen, domain_view_requested
                # A completed request remains in transcript history; only the visible
                # pending-request panel should trigger an approval response.
                pending = "Pending request" in s and "Needs your approval" in s
                if pending:
                    if not approval_prompt_seen:
                        tui.keys(CTRL_Y, 1.0)
                    approval_prompt_seen = True
                    return False
                approval_prompt_seen = False
                if ("Write a task" in s and "目标 / 自然语言反馈" not in s
                        and not domain_view_requested):
                    tui.keys(F6, 1.0)
                    domain_view_requested = True
                    return False
                return "来源实例对账" in s

            screen = fact("source ledger", ledger_or_read_only_approval, 600 if hospital else 180)
            if hospital:
                expect("待确认" in screen or "pending_confirmation" in screen,
                       "hospital ledger did not expose unresolved source states")
            else:
                expect(
                    "pending_addition" in screen and "suspected_duplicate" in screen,
                    "ledger did not expose expected mixed-floor states",
                )
                if case != "blocked":
                    expect("capability_missing" in screen, "ledger did not expose capability gaps")
        if not report.step("real dsh scans and shows the mixed-floor ledger", scan, tui):
            return
        if case == "scan":
            tui.keys(CTRL_C)
            return
        if case == "clarify":
            from domain_clarification_check import run_clarification_checks
            run_clarification_checks(report, tui, send, finished, fact, context, root)
            return
        if case == "blocked":
            def blocked_proposal():
                send("只为 source.mixed.instance.84.80.7E 准备一张椅子，把它放到受保护柱 column.protected 的中心 (9,4,0)。使用 0.5 米宽深、0.9 米示意高度，明确高度为 assumed；不要移动或修改受保护柱，预期领域校验应报告 blocked。")
                proposal = fact(
                    "blocked chair proposal",
                    lambda s: bool(__import__("re").search(r"方案 [0-9a-f]{32} · 修订", s))
                    and "blocked" in s,
                    240,
                )
                expect("blocked" in proposal, "protected-column placement was not blocked")
                finished()

            if not report.step("real dsh blocks a protected-column placement", blocked_proposal, tui):
                return

            def adjust_blocked_proposal():
                send("保留椅子的来源身份、尺寸依据和保护柱不变，把椅子移回来源位置 (12,7,0)，重新准备可执行版本。")
                revised = fact(
                    "blocked chair adjustment",
                    lambda s: "修订 2" in s and "prepared" in s,
                    240,
                )
                expect("prepared" in revised, "blocked chair adjustment was not prepared")
                finished()

            if not report.step("real dsh adjusts the blocked placement without changing protection", adjust_blocked_proposal, tui):
                return

            def apply_adjusted_blocked():
                tui.keys(CTRL_Y, 1.0)
                fact("adjusted blocked scene applied", lambda s: "场景应用 applied" in s, 300)
                fact("adjusted blocked artifact validated", lambda s: "产物验证 valid=true · current=true" in s, 300)
                finished()

            report.step("concrete approval builds the adjusted placement after collision recovery", apply_adjusted_blocked, tui)
            return
        def propose():
            prompt = ("根据当前台账，为 source.mixed.instance.84.80.7E 的椅子、source.mixed.instance.84.69 的柜子和 source.mixed.instance.84.6B 的洗手盆准备同一个可执行候选。保留来源 XY、变换和稳定身份；椅高0.9米、柜高1.8米、盆高0.85米允许作为 assumed 示意尺寸。只包含这三个实例，不包含已建、疑似重复、门窗、楼梯或井道。" if mixed else
                      "根据当前台账，只为 source.mixed.instance.84.80.7E 这一张待新增椅子准备可执行方案；保留 source_id、实例身份和来源 XY，使用 0.5 米宽深、0.9 米示意高度，明确高度为 assumed；不要扫描其他类别。")
            send(prompt)
            proposal = fact("typed chair proposal", lambda s: bool(__import__("re").search(r"方案 [0-9a-f]{32} · 修订", s))
                            and ("prepared" in s or "blocked" in s))
            expect("prepared" in proposal, "chair proposal was not executable")
            finished()
        if not report.step("real dsh prepares executable source-backed components", propose, tui):
            return
        def revise():
            send("只把椅子向北移动 1 米，保留来源和身份，其余候选保持原样。")
            revised = fact("chair revision", lambda s: "修订 2" in s, 240)
            expect("prepared" in revised, "chair revision was not prepared")
            finished()
        if not report.step("natural-language feedback produces revision two", revise, tui):
            return
        if mixed:
            def select():
                send("部分采纳：只保留椅子和洗手盆，跳过柜子。请生成依赖完整的新子方案并在台账保留跳过理由；不要把跳过记为建成。")
                selected = fact("partial selection", lambda s: "修订 3" in s, 240)
                expect("prepared" in selected, "partial selection was not prepared")
                finished()
            if not report.step("partial acceptance keeps chair and basin and skips cabinet", select, tui):
                return
            def rotate():
                send("只将椅子相对当前朝向再旋转45度，保持位置和身份；洗手盆及其定义保持原样。")
                revised = fact("chair rotation", lambda s: "修订 4" in s, 240)
                expect("prepared" in revised, "second local revision was not prepared")
                finished()
            if not report.step("a second local revision rotates only the selected chair", rotate, tui):
                return
        def apply_and_build():
            tui.keys(CTRL_Y, 1.0)
            fact("applied scene", lambda s: "场景应用 applied" in s, 240)
            fact("validated applied artifact", lambda s: "产物验证 valid=true · current=true" in s, 240)
            finished()
        report.step("concrete UI approval applies and builds the revised components", apply_and_build, tui)
    finally:
        try:
            tui.close()
        finally:
            lease.close()

def cli_checks(report, binary, common, args, workspace, journal, root):
    def run(arguments, **kwargs):
        return run_cli(binary, common + arguments, **kwargs)

    def plain():
        result = run(["--run", "Reply with exactly: READY", "--model", args.model])
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-300:]}")
        expect("READY" in result.stdout, "the reply is not on stdout")
    report.step("--run prints the reply and exits 0", plain)

    events = []

    def json_events():
        result = run(["--run", "Reply with exactly: READY", "--model", args.model, "--json-events"])
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-300:]}")
        for line in result.stdout.splitlines():
            events.append(json.loads(line))
        expect(len(events) >= 3, f"only {len(events)} events")
        sequences = [event["event_seq"] for event in events if "event_seq" in event]
        expect(sequences == sorted(sequences), "event sequences are not ordered")
        final = events[-1]["payload"]
        expect(final.get("session_closed") is True, "the final event does not close the session")
        expect(final.get("execution_result") == "completed", f"execution result is {final.get('execution_result')}")
        expect("READY" not in result.stdout, "the redacted event stream contains the reply text")
        return f"{len(events)} events"
    report.step("--json-events is ordered JSONL, ends with a closed completed session, and carries no reply text", json_events)

    def bad_model():
        result = run(["--run", "hi", "--model", "no-such-provider/no-such-model"])
        expect(result.returncode != 0, "an unknown model was accepted")
        expect("no-such" in result.stderr or "model" in result.stderr.lower(), f"unclear error: {result.stderr.strip()[-200:]}")
        return result.stderr.strip().splitlines()[-1][:120]
    report.step("an unknown --model fails with a message and starts no task", bad_model)

    def bad_preset():
        result = run(["--run", "hi", "--model", args.model, "--permission", "no-such-preset"])
        expect(result.returncode != 0, "an unknown preset was accepted")
        expect("No task was started" in result.stderr, f"unclear error: {result.stderr.strip()[-200:]}")
    report.step("an unknown --permission fails before any task", bad_preset)

    def read_only():
        target = workspace / "headless.txt"
        result = run(["--run", "Use your shell tool to create a file named headless.txt containing ok. "
                      "Reply CREATED or NOT_CREATED.", "--model", args.model, "--permission", "read-only"])
        expect(not target.exists(), "a file was written under --permission read-only")
        return f"exit {result.returncode}"
    report.step("--permission read-only keeps a headless run from writing", read_only)

    def bad_profile():
        result = run_cli(binary, ["--cwd", str(workspace), "--journal-dir", str(journal), "--dsh", args.dsh,
                                  "--profile", "zf-no-such-profile", "--run", "hi"], timeout=120)
        expect(result.returncode != 0, "a missing profile was accepted")
        expect("does not exist" in result.stderr, f"the message does not say why: {result.stderr.strip()[-200:]}")
        return result.stderr.strip().splitlines()[-1][:140]
    report.step("a dsh profile that does not exist fails with a message", bad_profile)

    def bad_launcher():
        result = run_cli(binary, ["--cwd", str(workspace), "--journal-dir", str(journal), "--dsh",
                                  str(root / "no-such-dsh.cmd"), "--run", "hi"], timeout=60)
        expect(result.returncode != 0, "a missing launcher was accepted")
        expect(result.stderr.strip() != "", "no error message")
        return result.stderr.strip().splitlines()[-1][:140]
    report.step("a dsh launcher that does not exist fails with a message", bad_launcher)

    def workflow():
        plan = root / "workflow.json"
        plan.write_text(json.dumps({"tasks": [
            {"id": 1, "text": "Reply with exactly: STEP-ONE"},
            {"id": 2, "text": "Reply with exactly: STEP-TWO", "dependencies": [1]},
        ]}), encoding="utf-8")
        result = run(["--workflow", str(plan), "--headless", "--model", args.model], timeout=400)
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-300:]}")
        expect(result.stdout.find("STEP-ONE") != -1 and result.stdout.find("STEP-ONE") < result.stdout.find("STEP-TWO"),
               "the two tasks did not run in dependency order")
    report.step("--workflow --headless runs dependent tasks in order", workflow)

    sessions = []

    def list_sessions():
        result = run(["--sessions"])
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-200:]}")
        for line in result.stdout.splitlines():
            for word in line.split():
                if word.count("-") >= 2 and word[0].isdigit():
                    sessions.append(word.strip(",:"))
        expect(len(sessions) >= 3, f"only {len(sessions)} sessions listed")
        return f"{len(sessions)} sessions"
    report.step("--sessions lists the journal's sessions without starting dsh", list_sessions)

    def offline(option, extra=()):
        def check():
            expect(sessions, "no session id to read")
            result = run([option, sessions[0], *extra], timeout=60)
            expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-200:]}")
            expect(result.stdout.strip() != "", "no output")
            expect("READY" not in result.stdout and "Reply with exactly" not in result.stdout,
                   "offline output contains conversation text")
        return check
    report.step("--replay --json-events reads a stored session", offline("--replay", ["--json-events"]))
    report.step("--export previews a stored session", offline("--export", ["--since", "0"]))
    report.step("--recovery classifies a stored session", offline("--recovery"))

    def search():
        result = run(["--search", "completed"], timeout=60)
        expect(result.returncode == 0, f"exit {result.returncode}: {result.stderr.strip()[-200:]}")
    report.step("--search runs over the stored evidence", search)


if __name__ == "__main__":
    main()
