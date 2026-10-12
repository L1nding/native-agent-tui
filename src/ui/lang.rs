//! Interface language. Chinese is the default; `NATIVE_AGENT_TUI_LANG=en` selects English.
//!
//! Only UI chrome is translated here. Text that Core, the server or the user
//! produced (messages, tool summaries, errors, identifiers) is shown as written.
use crate::state::SessionPhase;

#[cfg(test)]
thread_local! {
    static TEST_ZH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the interface is shown in Chinese.
pub(super) fn zh() -> bool {
    // 既有渲染测试以英文文案为契约；测试默认英文，需要中文时用 `with_zh`。
    #[cfg(test)]
    {
        // 预览用测试二进制运行，靠环境变量切到中文；普通测试不设置它。
        static PREVIEW_ZH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        TEST_ZH.with(std::cell::Cell::get)
            || *PREVIEW_ZH
                .get_or_init(|| std::env::var("TUI_PREVIEW_LANG").is_ok_and(|value| is_zh(&value)))
    }
    #[cfg(not(test))]
    {
        match CHOSEN.load(std::sync::atomic::Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => from_environment().unwrap_or(true),
        }
    }
}

/// 0 = not chosen in this run; 1 = Chinese; 2 = English.
#[cfg(not(test))]
static CHOSEN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The language named by `NATIVE_AGENT_TUI_LANG`, when it is set.
pub(super) fn from_environment() -> Option<bool> {
    std::env::var("NATIVE_AGENT_TUI_LANG")
        .ok()
        .map(|value| is_zh(&value))
}

/// Switches the interface language for the rest of this run.
pub(super) fn set(value: &str) {
    #[cfg(test)]
    TEST_ZH.with(|zh| zh.set(is_zh(value)));
    #[cfg(not(test))]
    CHOSEN.store(
        if is_zh(value) { 1 } else { 2 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The current language as a `Setting::Language` value.
pub(super) fn name() -> &'static str {
    if zh() {
        "zh"
    } else {
        "en"
    }
}

/// Unknown values keep the default language instead of silently switching.
fn is_zh(value: &str) -> bool {
    !value.trim().to_ascii_lowercase().starts_with("en")
}

/// Pick the wording for the current language. Both wordings stay at the call site.
pub(super) fn tr(en: &'static str, zh_text: &'static str) -> &'static str {
    if zh() {
        zh_text
    } else {
        en
    }
}

/// Session phase as shown in headers. English keeps the variant name.
pub(super) fn phase(phase: SessionPhase) -> String {
    if !zh() {
        return format!("{phase:?}");
    }
    match phase {
        SessionPhase::Created => "已创建",
        SessionPhase::Launching => "启动中",
        SessionPhase::Initializing => "初始化中",
        SessionPhase::CheckingShell => "检查 Shell",
        SessionPhase::Ready => "就绪",
        SessionPhase::StartingTurn => "开始本轮",
        SessionPhase::Running => "运行中",
        SessionPhase::GatePending => "等待子 Agent",
        SessionPhase::Completed => "已完成",
        SessionPhase::Interrupted => "已中断",
        SessionPhase::Stopping => "正在停止",
        SessionPhase::ClosingTransport => "正在断开",
        SessionPhase::Stopped => "已停止",
        SessionPhase::Disconnected => "连接已断开",
        SessionPhase::Failed => "失败",
        SessionPhase::Unknown => "状态未知",
    }
    .to_owned()
}

/// Debug-formatted fact for a detail row. English keeps Rust's `{:?}` text; the
/// Chinese interface drops the `Some(..)` wrapper and quotes and names `None`.
pub(super) fn show<T: std::fmt::Debug>(value: &T) -> String {
    let text = format!("{value:?}");
    if !zh() {
        return text;
    }
    if text == "None" {
        return "无".into();
    }
    let inner = text
        .strip_prefix("Some(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(&text);
    inner
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(inner)
        .to_owned()
}

/// Run `body` with the Chinese interface on this test thread.
#[cfg(test)]
pub(super) fn with_zh<T>(body: impl FnOnce() -> T) -> T {
    TEST_ZH.with(|flag| flag.set(true));
    let result = body();
    TEST_ZH.with(|flag| flag.set(false));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_is_the_default_and_only_an_english_value_switches() {
        assert!(is_zh("zh"));
        assert!(is_zh("zh-CN"));
        assert!(is_zh(""));
        assert!(is_zh("unknown"));
        assert!(!is_zh("en"));
        assert!(!is_zh(" EN_US "));
        assert_eq!(tr("Send", "发送"), "Send");
        assert_eq!(with_zh(|| tr("Send", "发送")), "发送");
        // 英文保留 Debug 文本这一既有契约；中文去掉 Some(..) 和引号。
        assert_eq!(show(&Some("root")), "Some(\"root\")");
        assert_eq!(show(&None::<u8>), "None");
        with_zh(|| {
            assert_eq!(show(&Some("root")), "root");
            assert_eq!(show(&Some(15000)), "15000");
            assert_eq!(show(&None::<u8>), "无");
            assert_eq!(show(&SessionPhase::Ready), "Ready");
        });
        assert_eq!(tr("Send", "发送"), "Send");
    }
}
