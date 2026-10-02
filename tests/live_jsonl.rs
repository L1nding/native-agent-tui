#[cfg(windows)]
#[test]
#[ignore = "requires Codex 0.159.2 and Python; all model traffic stays on localhost"]
fn live_cli_jsonl_matches_the_real_app_server_and_durable_replay() {
    let output = std::process::Command::new(
        std::env::var_os("NATIVE_AGENT_TUI_PYTHON").unwrap_or_else(|| "python".into()),
    )
    .arg(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/live_jsonl_codex_check.py"),
    )
    .arg("--binary")
    .arg(env!("CARGO_BIN_EXE_native-agent-tui"))
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
