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
                reason += f" (screen: {shot.name})"
            self.rows.append((name, "FAIL", reason, time.monotonic() - started))
        status = self.rows[-1]
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
    parser.add_argument("--only", choices=["tui", "cli", "domain"], help="run one half")
    args = parser.parse_args()
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
    if args.only != "cli":
        tui_checks(report, binary, common, args, workspace, code_word)
        time.sleep(2)
        report.step("no dsh process is left behind by the TUI runs",
                    lambda: expect(processes() <= before, f"extra dsh processes: {sorted(processes() - before)}"))
    if args.only != "tui":
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



def domain_checks(report, binary, common, args, workspace, root):
    fixture = Path(os.environ.get("MODELING_COMPLEX_FIXTURE", ""))
    context = Path(os.environ.get("MODELING_COMPLEX_CONTEXT", str(fixture / "context.json")))
    repository = Path(os.environ.get("MODELING_COMPLEX_ROOT", str(fixture)))
    if not context.is_file():
        report.step("complex fixture context exists", lambda: expect(False, f"missing {context}"))
        return
    domain_journal = root / "domain-journal"
    domain_journal.mkdir(parents=True, exist_ok=True)
    command = [str(binary), "--cwd", str(repository), "--domain-context", str(context),
               "--model", args.model, "--journal-dir", str(domain_journal),
               "--dsh", args.dsh, "--profile", args.profile]
    tui = Tui(command, repository, report.evidence)
    try:
        def run_domain():
            tui.wait("modeling session ready", lambda s: "建模" in s and "Initializing" not in s, 120)
            tui.type("开始混合楼层建模。请先用 modeling_domain.describe_capabilities，再调用 scan_source_instances 分页扫描并展示来源实例对账；保留已有、待新增、疑似重复、待确认和缺能力状态，先不要提交构建方案。")
            tui.keys(ENTER, 0.5)
            screen = tui.wait("source ledger", lambda s: "来源实例对账" in s or "source_ledger" in s, 180)
            expect("pending_addition" in screen and "suspected_duplicate" in screen,
                   "ledger did not expose expected mixed-floor states")
            if os.environ.get("MODELING_COMPLEX_FULL") != "1":
                tui.keys(CTRL_C)
                return
            tui.type("根据当前台账，只为 source.mixed.instance.84.80.7E 这一张待新增椅子准备可执行方案；保留 source_id、实例身份和来源 XY，使用 0.5 米宽深、0.9 米示意高度，明确高度为 assumed；不要扫描其他类别。")
            tui.keys(ENTER, 0.5)
            proposal = tui.wait("typed chair proposal", lambda s: bool(__import__("re").search(r"方案 [0-9a-f]{32} · 修订", s)) and ("prepared" in s or "blocked" in s), 300)
            expect("prepared" in proposal, "chair proposal was not executable")
            tui.type("只把这张椅子向北移动 1 米，保留来源和身份")
            tui.keys(ENTER, 0.5)
            revised = tui.wait("chair revision", lambda s: "修订 2" in s, 180)
            expect("prepared" in revised, "chair revision was not prepared")
            tui.keys(CTRL_Y, 1.0)
            tui.wait("approval result", lambda s: "applied" in s or "授权" in s or "具体版本" in s, 180)
            tui.keys(CTRL_C)
        report.step("real dsh scans and shows the mixed-floor ledger", run_domain, tui)
    finally:
        tui.close()

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
