//! Business-language reading of tool rows. Display only: Core keeps the raw
//! summary text, which history, search and the journal use unchanged.

use super::lang;
use crate::tool_details::{ConversationTool, ToolLifecycle};

/// The tool's own name without the MCP server prefix (`server/name`, `mcp__server__name`).
fn short_name(name: &str) -> &str {
    let name = name.rsplit('/').next().unwrap_or(name);
    name.rsplit("__").next().unwrap_or(name)
}

/// What a modeling, goal or todo tool does, in the user's words.
fn label(name: &str) -> Option<&'static str> {
    Some(match short_name(name) {
        "inspect_project" => "读取项目目录",
        "select_project_target" => "选择建模目标",
        "inspect_project_floor_location" => "读取楼层定位",
        "prepare_project_floor" => "准备楼层结构方案",
        "inspect_project_floor" => "恢复楼层状态",
        "activate_project_floor" => "切换楼层",
        "discard_incomplete_floor" => "清理未完成的初始化",
        "describe_capabilities" => "读取建模能力",
        "scan_source_instances" => "扫描构件来源",
        "scan_source_groups" => "汇总构件分组",
        "interpret_source_group" => "识别构件分组",
        "interpret_source_instance" => "识别单个构件",
        "record_source_disposition" => "记录来源处理结果",
        "select_candidate_components" => "选择候选构件",
        "inspect_existing_components" => "读取已有构件",
        "read_fact_view" => "读取详细事实",
        "prepare_change" => "准备构件方案",
        "prepare_group_change" => "准备分组构件方案",
        "prepare_component_edit" => "准备构件修改",
        "prepare_drawing_correction" => "准备图纸修正",
        "prepare_proxy_test_batch" => "准备代理测试批次",
        "inspect_proxy_test_decisions" => "读取代理测试决定",
        "inspect_change" => "读取方案",
        "apply_change" => "应用方案",
        "inspect_operation" => "查询应用结果",
        "build_scene" => "构建模型",
        "preview_candidate" => "生成候选预览",
        "inspect_artifact" => "查询构建任务",
        "cancel_artifact" => "取消构建任务",
        "validate_artifact" => "验证构建产物",
        "render_artifact_review" => "渲染复核图",
        "read_artifact_review_image" => "查看复核图",
        "audit_drawing" => "审查图纸",
        "render_drawing_candidate" => "渲染图纸疑点",
        "read_drawing_candidate_image" => "查看图纸疑点",
        "record_drawing_judgement" => "记录图纸判断",
        "assemble_project_building" => "组装整栋模型",
        "inspect_project_building" => "查询整栋组装",
        "cancel_project_building" => "取消整栋组装",
        "validate_project_building" => "验证整栋模型",
        "check_project_building_connections" => "检查楼层连接",
        "read_project_building_report" => "读取整栋报告",
        "render_project_building_review" => "渲染整栋复核图",
        "read_project_building_review_image" => "查看整栋复核图",
        "activate_project_connection_scope" => "切换连接作用域",
        "inspect_project_connection_scope" => "恢复连接作用域",
        "prepare_project_connection_change" => "准备连接方案",
        "create_goal" => "设定目标",
        "get_goal" => "读取目标",
        "update_goal" => "更新目标",
        "todo_write" => "更新待办",
        _ => return None,
    })
}

/// Tools whose output the user does not need in the preview (goal and todo state).
pub(super) fn quiet(tool: &ConversationTool) -> bool {
    tool.name.as_deref().map(short_name).is_some_and(|name| {
        matches!(
            name,
            "create_goal" | "get_goal" | "update_goal" | "todo_write"
        )
    })
}

/// Typical duration of a step that is slow by nature, so a long wait reads as normal.
fn slow_hint(name: &str) -> Option<&'static str> {
    match short_name(name) {
        "prepare_project_floor" => Some("大图纸通常需要 1–3 分钟"),
        "build_scene" | "preview_candidate" | "assemble_project_building" => {
            Some("构建耗时取决于图纸和构件数量")
        }
        _ => None,
    }
}

/// One line for a modeling tool's sealed receipt, instead of its raw JSON.
pub(super) fn result_summary(tool: &ConversationTool) -> Option<String> {
    if !lang::zh() || tool.name.as_deref().and_then(label).is_none() {
        return None;
    }
    let output = tool.output.as_deref()?;
    let value: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
    let kind = value["kind"].as_str()?;
    let what = match kind {
        "project_catalog" => "项目目录",
        "project_selection" => "建模目标",
        "project_location" => "楼层定位",
        "project_scope" => "楼层结构方案",
        "project_floor_discarded" => "已清理未完成的初始化",
        "proposal" => "构件方案",
        "operation" => "应用回执",
        "artifact" => "构建任务",
        "artifact_validation" => "产物验证结果",
        "source_ledger" => "构件来源",
        "source_groups" => "构件分组",
        "drawing_audit" => "图纸审查结果",
        "capabilities" => "建模能力",
        "diagnostic" => "需要处理的问题",
        _ => return None,
    };
    Some(if value["ok"] == false || kind == "diagnostic" {
        format!("→ {what}（详情见 F6 建模页）")
    } else {
        format!("→ 得到：{what}")
    })
}

/// A Core tool row as the Chinese UI reads it: `▸ 运行中 · 准备楼层结构方案 · 已运行 46s`.
pub(super) fn localized_line(line: &str, tool: Option<&ConversationTool>) -> String {
    if !lang::zh() {
        return line.to_owned();
    }
    let Some(body) = line.strip_prefix("▸ ") else {
        return line.to_owned();
    };
    let name = tool.and_then(|tool| tool.name.as_deref());
    let mut parts = body.split(" · ");
    let state = match parts.next().unwrap_or_default() {
        "running" => "运行中",
        "done" => "完成",
        "failed" => "失败",
        "interrupted" => "已中断",
        "ended, outcome unknown" => "已结束，结果未知",
        "unknown" => "未知",
        other => other,
    }
    .to_owned();
    let mut shown = vec![state];
    for part in parts {
        if let Some(code) = part.strip_prefix("exit ") {
            shown.push(format!("退出码 {code}"));
        } else if let Some(seconds) = part.strip_suffix("s elapsed") {
            shown.push(format!("已运行 {seconds}s"));
        } else if let Some(text) = name
            .filter(|name| crate::state::display_text(name) == part)
            .and_then(label)
        {
            shown.push(text.to_owned());
        } else {
            shown.push(part.to_owned());
        }
    }
    let mut text = format!("▸ {}", shown.join(" · "));
    if let Some(hint) = tool
        .filter(|tool| tool.lifecycle == ToolLifecycle::Running)
        .and(name)
        .and_then(slow_hint)
    {
        text.push_str(&format!("（{hint}）"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCategory;

    fn tool(name: &str, lifecycle: ToolLifecycle, output: Option<&str>) -> ConversationTool {
        ConversationTool {
            item_id: "t".into(),
            category: ToolCategory::Mcp,
            lifecycle,
            name: Some(name.into()),
            command: None,
            cwd: None,
            target: None,
            exit_code: None,
            output_lines: None,
            added_lines: None,
            removed_lines: None,
            output: output.map(Into::into),
            output_clipped: false,
        }
    }

    #[test]
    fn modeling_rows_read_as_business_actions_with_elapsed_time_and_slow_hints() {
        lang::set("zh");
        let running = tool(
            "modeling_domain/prepare_project_floor",
            ToolLifecycle::Running,
            None,
        );
        assert_eq!(
            localized_line(
                "▸ running · modeling_domain/prepare_project_floor · 46s elapsed",
                Some(&running)
            ),
            "▸ 运行中 · 准备楼层结构方案 · 已运行 46s（大图纸通常需要 1–3 分钟）"
        );
        let done = tool(
            "mcp__modeling_domain__inspect_project",
            ToolLifecycle::Completed,
            None,
        );
        assert_eq!(
            localized_line(
                "▸ done · 0.3s · mcp__modeling_domain__inspect_project",
                Some(&done)
            ),
            "▸ 完成 · 0.3s · 读取项目目录"
        );
        let shell = tool("shell", ToolLifecycle::Failed, None);
        assert_eq!(
            localized_line("▸ failed · exit 1 · cargo test", Some(&shell)),
            "▸ 失败 · 退出码 1 · cargo test"
        );
        lang::set("en");
        assert_eq!(
            localized_line("▸ done · get_goal", Some(&done)),
            "▸ done · get_goal"
        );
    }

    #[test]
    fn modeling_receipts_summarize_their_kind_and_goal_tools_stay_quiet() {
        lang::set("zh");
        let scope = tool(
            "modeling_domain/prepare_project_floor",
            ToolLifecycle::Completed,
            Some(r#"{"contract":"modeling-fact-receipt/v1","kind":"project_scope","ok":true}"#),
        );
        assert_eq!(
            result_summary(&scope).as_deref(),
            Some("→ 得到：楼层结构方案")
        );
        let rejected = tool(
            "modeling_domain/prepare_project_floor",
            ToolLifecycle::Completed,
            Some(r#"{"kind":"diagnostic","ok":false}"#),
        );
        assert_eq!(
            result_summary(&rejected).as_deref(),
            Some("→ 需要处理的问题（详情见 F6 建模页）")
        );
        assert!(result_summary(&tool("shell", ToolLifecycle::Completed, Some("{}"))).is_none());
        assert!(quiet(&tool("update_goal", ToolLifecycle::Completed, None)));
        assert!(!quiet(&scope));
    }
}
