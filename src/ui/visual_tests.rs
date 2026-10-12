//! Deterministic terminal previews: fixed data only, no provider or live session.
use super::*;
use crate::agents::{AgentInfo, AgentSnapshot};
use crate::state::{ConversationItem, SessionPhase};
use ratatui::{
    backend::TestBackend,
    style::{Color, Modifier},
    Terminal,
};
use std::fmt::Write;

fn fixture(phase: SessionPhase) -> CoreSnapshot {
    let mut snapshot = CoreSnapshot {
        phase,
        thread_id: Some("root".into()),
        model: Some("codex".into()),
        cwd: "D:/workspace/native-agent".into(),
        sandbox: "workspace-write".into(),
        approval_policy: "on-request".into(),
        root_turn_count: 1,
        ..Default::default()
    };
    if phase != SessionPhase::Ready {
        for (role, text) in [
            ("You", "Review the interface and make the execution flow easier to follow."),
            ("Agent", "I will inspect the current layout and trace the interaction paths.\n\n## Interface review\n\n- Keep the conversation as the main reading surface.\n- Make pending requests easy to notice.\n- Preserve keyboard navigation and search anchors."),
            // 与 Core 的 `tool_details::summary_line` 同一格式，超过折叠上限。
            ("Tool", "▸ done · exit 0 · 0.2s · rg -n \"fn draw\" src/ui\n▸ done · exit 0 · 0.1s · Get-Content src/ui/render.rs\n▸ done · exit 0 · 0.1s · Get-Content src/ui/layout.rs\n▸ failed · exit 101 · 8.4s · cargo test --locked ui::layout\n▸ done · exit 0 · 3.9s · cargo check --locked --all-targets\n▸ done · exit 0 · 0.3s · git diff --stat"),
            ("Agent", "The interface already has useful navigation tools. The next step is a calmer visual hierarchy with clear focus and readable status information.\n\nUse `workspace_layout` for both painting and search, so **one geometry** drives every view:\n\n```rust\nlet (agents, conversation) = workspace_layout(area, has_agents);\nlet (width, height) = conversation_content_size(conversation);\n```\n\n> 中文与 emoji 🙂 也按显示宽度换行。"),
        ] {
            snapshot.messages.push(ConversationItem { id: snapshot.messages.len().to_string(), thread_id: "root".into(), turn_id: "turn-1".into(), role: role.into(), text: text.into(), complete: true, truncated: false });
        }
        snapshot.agents.push(AgentSnapshot {
            info: AgentInfo {
                id: "reviewer".into(),
                parent_id: "root".into(),
                path: None,
                nickname: Some("UI reviewer".into()),
                role: Some("review".into()),
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: Some("review-1".into()),
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        });
    }
    if phase == SessionPhase::Failed {
        snapshot.last_error = Some(
            "Connection interrupted. Review the last confirmed result before continuing.".into(),
        );
    }
    if matches!(
        std::env::var("TUI_PREVIEW_SCENE").as_deref(),
        Ok("approval") | Ok("request-panel")
    ) {
        snapshot.requests.push(crate::interactions::RequestView::decode(
            crate::protocol::RpcId::Number(1), "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"root","turnId":"turn-1","command":"cargo check --locked --all-targets","reason":"Verify the interface changes."}),
        ).unwrap());
    }
    if phase == SessionPhase::Running {
        use crate::observation::{ObservationFacts, Observer};
        use crate::scheduler::{ExternalTurn, Scheduler};
        let now = tokio::time::Instant::now();
        let mut scheduler = Scheduler::default();
        scheduler
            .enqueue(vec![RootTaskSpec::input("Synthetic preview task".into())])
            .unwrap();
        let attempt = scheduler.dispatch().unwrap().attempt;
        scheduler
            .started_root(
                attempt,
                ExternalTurn {
                    thread_id: "root".into(),
                    turn_id: "turn-1".into(),
                    generation: 1,
                },
            )
            .unwrap();
        let mut observer = Observer::new_at("visual-preview".into(), Default::default(), now, None);
        observer
            .reconcile(
                ObservationFacts {
                    phase,
                    root_thread: Some("root"),
                    root_generation: 1,
                    root_task: scheduler.task(attempt.task),
                    children: vec![],
                    requests: &snapshot.requests,
                    gate: None,
                },
                now,
            )
            .unwrap();
        snapshot.observation = observer.snapshot_at(1, now + std::time::Duration::from_secs(12));
        snapshot.timeline = observer.timeline_snapshot();
    }
    snapshot
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn css(color: Color, foreground: bool) -> String {
    match color {
        Color::Reset => if foreground { "#dce2eb" } else { "#15191f" }.into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => {
            let ansi = [
                "#101418", "#dc6868", "#78bd94", "#e4be77", "#75a5dc", "#b29ad8", "#74c9ca",
                "#d3d9e2", "#738093", "#ee9090", "#a0d4ad", "#efd79e", "#99bfee", "#c6b1e7",
                "#9ae0df", "#f5f7fa",
            ];
            if i < 16 {
                ansi[i as usize].into()
            } else if i >= 232 {
                let v = 8 + (i - 232) * 10;
                format!("#{v:02x}{v:02x}{v:02x}")
            } else {
                let n = i - 16;
                let v = |c| if c == 0 { 0 } else { 55 + c * 40 };
                format!("#{:02x}{:02x}{:02x}", v(n / 36), v(n / 6 % 6), v(n % 6))
            }
        }
        other => {
            let index = match other {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::DarkGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                _ => 15,
            };
            css(Color::Indexed(index), foreground)
        }
    }
}

#[test]
#[ignore = "writes a deterministic HTML terminal preview to target/ui-previews"]
fn export_visual_preview() {
    let name = std::env::var("TUI_PREVIEW_NAME").unwrap_or_else(|_| "current".into());
    assert!(name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    let mut html = String::from("<!doctype html><html lang=en><meta charset=utf-8><title>Native Agent terminal review</title><style>body{background:#101318;color:#e3e7ed;font:14px system-ui;margin:32px}h1{font-size:25px}h2{font-size:16px;margin:28px 0 12px}.terminal{position:relative;background:#15191f;border:1px solid #39414e;border-radius:8px;overflow:hidden}.cell{position:absolute;white-space:pre;font:14px/20px 'Cascadia Mono',Consolas,monospace;height:20px}</style><h1>Native Agent · terminal review</h1><p>Fixed synthetic data rendered by Ratatui TestBackend. No live session content.</p>");
    for (label, phase, width, height, palette) in [
        ("Empty / 80 x 24", SessionPhase::Ready, 80, 24, false),
        ("Running / 120 x 32", SessionPhase::Running, 120, 32, false),
        ("Failed / 80 x 24", SessionPhase::Failed, 80, 24, false),
        ("Unknown / 80 x 24", SessionPhase::Unknown, 80, 24, false),
        ("Commands / 100 x 24", SessionPhase::Running, 100, 24, true),
    ] {
        let snapshot = fixture(phase);
        let mut local = LocalState::default();
        if palette {
            local.palette.toggle();
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &snapshot, &local))
            .unwrap();
        write!(
            html,
            "<h2>{label}</h2><div class=terminal style='width:{}px;height:{}px'>",
            width * 9,
            height * 20
        )
        .unwrap();
        for (index, cell) in terminal.backend().buffer().content().iter().enumerate() {
            let x = index % width as usize;
            let y = index / width as usize;
            let reversed = cell.modifier.contains(Modifier::REVERSED);
            let (fg, bg) = if reversed {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            write!(html,"<span class=cell style='left:{}px;top:{}px;min-width:9px;color:{};background:{};font-weight:{}'>{}</span>",x*9,y*20,css(fg,true),css(bg,false),if cell.modifier.contains(Modifier::BOLD){700}else{400},escape(cell.symbol())).unwrap();
        }
        html.push_str("</div>");
    }
    html.push_str("</html>");
    std::fs::create_dir_all("target/ui-previews").unwrap();
    std::fs::write(format!("target/ui-previews/{name}.html"), html).unwrap();
}

/// Opens the real terminal renderer using synthetic data and the production key router.
/// Commands go to a local sink; no client, transport, credentials or provider is opened.
#[tokio::test]
#[ignore = "interactive terminal preview; run with --ignored --nocapture --test-threads=1"]
async fn interactive_visual_preview() {
    // Broad --ignored verification must never open a terminal or wait for input.
    if std::env::var("TUI_INTERACTIVE_PREVIEW").as_deref() != Ok("1") {
        return;
    }
    use super::input::InputEvent;
    use crossterm::event::KeyEventKind;
    use std::time::{Duration, Instant};
    let phase = match std::env::var("TUI_PREVIEW_SCENE").as_deref() {
        Ok("empty") => SessionPhase::Ready,
        Ok("failed") => SessionPhase::Failed,
        Ok("unknown") => SessionPhase::Unknown,
        _ => SessionPhase::Running,
    };
    let snapshot = fixture(phase);
    let mut guard = super::terminal::TerminalGuard::enter().unwrap();
    let mut local = LocalState::default();
    if std::env::var("TUI_PREVIEW_SCENE").as_deref() == Ok("commands") {
        local.palette.toggle();
    }
    if std::env::var("TUI_PREVIEW_SCENE").as_deref() == Ok("request-panel") {
        local.request_panel = true;
    }
    if std::env::var("TUI_PREVIEW_SCENE").as_deref() == Ok("help") {
        local.help = true;
    }
    if std::env::var("TUI_PREVIEW_SCENE").as_deref() == Ok("tools") {
        local.tools_expanded = true;
    }
    match std::env::var("TUI_PREVIEW_SCENE").as_deref() {
        Ok("timeline") => local.timeline.open("root".into(), &snapshot),
        Ok("search") => local.search.open("root".into()),
        Ok("evidence") => local.evidence = true,
        Ok("workflow") => local.workflow.visible = true,
        Ok("context") => local.context.toggle(),
        Ok("skills") => local.skills = true,
        _ => {}
    }
    if std::env::var("TUI_PREVIEW_SCENE").as_deref() == Ok("input") {
        local.editor.insert("Review the implementation.\nKeep the interface clear and keyboard friendly.\nCheck 中文 and emoji 🙂 alignment.");
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let mut dirty = true;
    while Instant::now() < deadline {
        if let Ok(control) = std::env::var("TUI_PREVIEW_CONTROL") {
            if std::fs::read_to_string(control).is_ok_and(|value| value.trim() == "close") {
                return;
            }
        }
        if dirty {
            let size = guard.terminal.size().unwrap();
            local.viewport = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            sync_local_requests(&mut local, &snapshot);
            guard
                .terminal
                .draw(|frame| render::draw(frame, &snapshot, &local))
                .unwrap();
            dirty = false;
        }
        for _ in 0..128 {
            let Some(event) = guard.input.read_ready().unwrap() else {
                break;
            };
            match event {
                InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(key, &snapshot, &mut local, &tx) {
                        return;
                    }
                }
                InputEvent::Paste(text) => handle_paste(&text, &snapshot, &mut local),
                InputEvent::Resize(_, _) => {}
                _ => continue,
            }
            dirty = true;
        }
        while rx.try_recv().is_ok() {
            local.notice = Some("Preview only: execution commands are not dispatched.".into());
            dirty = true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
