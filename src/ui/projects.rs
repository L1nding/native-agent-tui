//! Project picker: open a modeler project, keep a plain session, or create a
//! project from one DXF drawing. Choosing only names the next session; the
//! runtime restarts the backend with it, and Core owns everything after that.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, selected_line};
use super::theme;
use crate::domain::{NewProject, ProjectEntry, PROJECT_UNITS};

/// What the next session is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectChoice {
    /// `--domain-project projects/<dir>`.
    Project(String),
    /// The `--domain-context` file this TUI was launched with.
    Context(PathBuf),
    /// No modeling tools: an ordinary dsh session.
    Plain,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProjectsEvent {
    Close,
    Consumed,
    Choose(ProjectChoice),
    Create(NewProject),
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Project(usize),
    Launch,
    Plain,
    New,
}

const FIELDS: [&str; 4] = ["项目名称", "项目描述", "DXF 图纸路径", "图纸单位"];

#[derive(Default)]
struct Form {
    name: String,
    description: String,
    source: String,
    units: usize,
    focus: usize,
}

impl Form {
    fn text(&mut self) -> Option<&mut String> {
        match self.focus {
            0 => Some(&mut self.name),
            1 => Some(&mut self.description),
            2 => Some(&mut self.source),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct ProjectsPanel {
    pub visible: bool,
    /// `/project` was typed; the runtime lists projects and opens the panel.
    pub request_open: bool,
    /// Startup page: Esc quits instead of returning to a session.
    startup: bool,
    entries: Vec<ProjectEntry>,
    launch: Option<PathBuf>,
    current: Option<ProjectChoice>,
    selected: usize,
    form: Option<Form>,
    pub notice: Option<String>,
}

impl ProjectsPanel {
    pub fn open(
        &mut self,
        entries: Vec<ProjectEntry>,
        launch: Option<PathBuf>,
        current: Option<ProjectChoice>,
        startup: bool,
    ) {
        self.entries = entries;
        self.launch = launch;
        self.startup = startup;
        self.form = None;
        self.notice = None;
        self.visible = true;
        self.request_open = false;
        let items = self.items();
        self.selected = current
            .as_ref()
            .and_then(|choice| {
                items
                    .iter()
                    .position(|item| self.choice(*item).as_ref() == Some(choice))
            })
            .unwrap_or(0);
        self.current = current;
    }

    /// After a project was created elsewhere, show it selected.
    pub fn refresh(&mut self, entries: Vec<ProjectEntry>, select: &str) {
        self.entries = entries;
        self.form = None;
        if let Some(index) = self.entries.iter().position(|entry| entry.path == select) {
            self.selected = index;
        }
    }

    fn items(&self) -> Vec<Item> {
        (0..self.entries.len())
            .map(Item::Project)
            .chain(self.launch.is_some().then_some(Item::Launch))
            .chain([Item::Plain, Item::New])
            .collect()
    }

    fn choice(&self, item: Item) -> Option<ProjectChoice> {
        match item {
            Item::Project(index) => Some(ProjectChoice::Project(self.entries[index].path.clone())),
            Item::Launch => self.launch.clone().map(ProjectChoice::Context),
            Item::Plain => Some(ProjectChoice::Plain),
            Item::New => None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(field) = self.form.as_mut().and_then(Form::text) {
            // Paths are often pasted quoted or with a trailing newline.
            field.push_str(text.trim().trim_matches('"').lines().next().unwrap_or(""));
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> ProjectsEvent {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && matches!(key.code, KeyCode::Char('c' | 'q')) {
            return if self.startup {
                ProjectsEvent::Quit
            } else {
                self.visible = false;
                ProjectsEvent::Close
            };
        }
        if let Some(form) = self.form.as_mut() {
            match key.code {
                KeyCode::Esc => self.form = None,
                KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % FIELDS.len(),
                KeyCode::BackTab | KeyCode::Up => {
                    form.focus = (form.focus + FIELDS.len() - 1) % FIELDS.len()
                }
                KeyCode::Left if form.focus == 3 => {
                    form.units = (form.units + PROJECT_UNITS.len() - 1) % PROJECT_UNITS.len()
                }
                KeyCode::Right | KeyCode::Char(' ') if form.focus == 3 => {
                    form.units = (form.units + 1) % PROJECT_UNITS.len()
                }
                KeyCode::Backspace => {
                    if let Some(field) = form.text() {
                        field.pop();
                    }
                }
                KeyCode::Enter => {
                    return ProjectsEvent::Create(NewProject {
                        name: form.name.trim().to_owned(),
                        description: form.description.trim().to_owned(),
                        source: form.source.trim().to_owned(),
                        units: PROJECT_UNITS[form.units].to_owned(),
                    });
                }
                KeyCode::Char(c) if !control => {
                    if let Some(field) = form.text() {
                        if field.chars().count() < 500 {
                            field.push(c);
                        }
                    }
                }
                _ => {}
            }
            return ProjectsEvent::Consumed;
        }
        let items = self.items();
        match key.code {
            KeyCode::Esc if self.startup => ProjectsEvent::Quit,
            KeyCode::Esc => {
                self.visible = false;
                ProjectsEvent::Close
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ProjectsEvent::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(items.len() - 1);
                ProjectsEvent::Consumed
            }
            KeyCode::Enter => match items.get(self.selected).copied() {
                Some(Item::New) => {
                    self.form = Some(Form::default());
                    self.notice = None;
                    ProjectsEvent::Consumed
                }
                Some(item) => self
                    .choice(item)
                    .map_or(ProjectsEvent::Consumed, ProjectsEvent::Choose),
                None => ProjectsEvent::Consumed,
            },
            _ => ProjectsEvent::Consumed,
        }
    }

    fn rows(&self, width: usize) -> Vec<Line<'static>> {
        let palette = theme::current();
        if let Some(form) = &self.form {
            let mut lines = vec![Line::from(Span::styled("新建项目", palette.heading()))];
            for (index, label) in FIELDS.iter().enumerate() {
                let value = match index {
                    0 => form.name.clone(),
                    1 => form.description.clone(),
                    2 => form.source.clone(),
                    _ => format!(
                        "◀ {} ▶",
                        ["按 DXF 声明（自动）", "毫米", "厘米", "米"][form.units]
                    ),
                };
                let cursor = if index == form.focus && index < 3 {
                    "▏"
                } else {
                    ""
                };
                let spans = vec![
                    Span::styled(format!("{label:　<6} "), palette.label()),
                    Span::raw(format!("{value}{cursor}")),
                ];
                lines.push(if index == form.focus {
                    selected_line(spans, width)
                } else {
                    Line::from(clip_spans(spans, width))
                });
            }
            lines.push(Line::from(Span::styled(
                "导入保留原图；打开项目后配置 /model，再输入“开始首次建模”确认范围与尺寸",
                palette.faint(),
            )));
            return lines;
        }
        let mut lines = vec![Line::from(Span::styled(
            if self.startup {
                "选择建模项目"
            } else {
                "切换建模项目（将结束当前会话）"
            },
            palette.heading(),
        ))];
        for (index, item) in self.items().into_iter().enumerate() {
            let current = self.choice(item).is_some() && self.choice(item) == self.current;
            let (title, detail) = match item {
                Item::Project(i) => {
                    let entry = &self.entries[i];
                    let dir = entry.path.trim_start_matches("projects/").to_owned();
                    (dir, entry.description.clone())
                }
                Item::Launch => (
                    "启动时的楼层上下文".to_owned(),
                    "支持 Blender 预览".to_owned(),
                ),
                Item::Plain => ("不使用建模".to_owned(), "普通 dsh 会话".to_owned()),
                Item::New => ("＋ 新建项目".to_owned(), "从一张 DXF 图纸创建".to_owned()),
            };
            let marker = if current { "● " } else { "  " };
            let pad = 24usize.saturating_sub(title.width());
            let spans = vec![
                Span::raw(format!("{marker}{title}{}", " ".repeat(pad))),
                Span::styled(
                    if current {
                        format!("{detail}（当前）")
                    } else {
                        detail
                    },
                    palette.secondary(),
                ),
            ];
            lines.push(if index == self.selected {
                selected_line(spans, width)
            } else {
                Line::from(clip_spans(spans, width))
            });
        }
        if self.entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "当前目录下没有 projects/*/project.yaml",
                palette.faint(),
            )));
        }
        lines
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(88);
        let body = self.rows(usize::from(width.saturating_sub(2)));
        let notice = self
            .notice
            .as_deref()
            .map(|text| Line::from(Span::styled(text.to_owned(), theme::current().secondary())));
        let height = (body.len() as u16 + 4 + u16::from(notice.is_some()) * 2)
            .min(area.height.saturating_sub(2))
            .max(6);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 3,
            width,
            height,
        };
        let hint = if self.form.is_some() {
            "Tab/↑↓ 切换字段 · ←→ 选单位 · Enter 创建 · Esc 返回列表"
        } else if self.startup {
            "↑↓ 选择 · Enter 打开 · Esc 退出"
        } else {
            "↑↓ 选择 · Enter 切换 · Esc 取消"
        };
        let mut lines = body;
        if let Some(notice) = notice {
            lines.push(Line::default());
            lines.push(notice);
        }
        lines.push(Line::default());
        lines.push(theme::hints(hint));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(theme::focus_panel().title(" 项目 ")),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn entries() -> Vec<ProjectEntry> {
        ["demo-building", "hospital-site"]
            .map(|dir| ProjectEntry {
                path: format!("projects/{dir}"),
                name: dir.into(),
                description: format!("{dir} 描述"),
            })
            .into()
    }

    #[test]
    fn current_project_is_preselected_and_choosing_another_names_the_next_session() {
        let mut panel = ProjectsPanel::default();
        let current = ProjectChoice::Project("projects/hospital-site".into());
        panel.open(entries(), None, Some(current), false);
        assert_eq!(panel.selected, 1);
        assert_eq!(panel.key(key(KeyCode::Up)), ProjectsEvent::Consumed);
        assert_eq!(
            panel.key(key(KeyCode::Enter)),
            ProjectsEvent::Choose(ProjectChoice::Project("projects/demo-building".into()))
        );
        panel.key(key(KeyCode::Down));
        panel.key(key(KeyCode::Down));
        assert_eq!(
            panel.key(key(KeyCode::Enter)),
            ProjectsEvent::Choose(ProjectChoice::Plain)
        );
        assert_eq!(panel.key(key(KeyCode::Esc)), ProjectsEvent::Close);
        assert!(!panel.visible);
    }

    #[test]
    fn launch_context_stays_selectable_and_startup_escape_quits() {
        let mut panel = ProjectsPanel::default();
        let launch = PathBuf::from("ctx.json");
        panel.open(Vec::new(), Some(launch.clone()), None, true);
        assert_eq!(
            panel.key(key(KeyCode::Enter)),
            ProjectsEvent::Choose(ProjectChoice::Context(launch))
        );
        assert_eq!(panel.key(key(KeyCode::Esc)), ProjectsEvent::Quit);
    }

    #[test]
    fn new_project_form_collects_typed_and_pasted_fields_without_choosing() {
        let mut panel = ProjectsPanel::default();
        panel.open(entries(), None, None, false);
        for _ in 0..3 {
            panel.key(key(KeyCode::Down));
        }
        assert_eq!(panel.key(key(KeyCode::Enter)), ProjectsEvent::Consumed);
        for c in "clinic-a".chars() {
            panel.key(key(KeyCode::Char(c)));
        }
        panel.key(key(KeyCode::Tab));
        panel.paste("诊所\n");
        panel.key(key(KeyCode::Tab));
        panel.paste("\"D:\\cad\\一层.dxf\"");
        panel.key(key(KeyCode::Tab));
        panel.key(key(KeyCode::Right));
        assert_eq!(
            panel.key(key(KeyCode::Enter)),
            ProjectsEvent::Create(NewProject {
                name: "clinic-a".into(),
                description: "诊所".into(),
                source: "D:\\cad\\一层.dxf".into(),
                units: "millimeters".into(),
            })
        );
        assert_eq!(panel.key(key(KeyCode::Esc)), ProjectsEvent::Consumed);
        assert!(panel.visible, "Esc in the form returns to the list");
    }

    #[test]
    fn picker_renders_choices_and_marks_the_current_session() {
        let mut panel = ProjectsPanel::default();
        let current = ProjectChoice::Project("projects/hospital-site".into());
        panel.open(entries(), None, Some(current), false);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| panel.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .replace(' ', "");
        for label in [
            "切换建模项目",
            "hospital-site",
            "（当前）",
            "不使用建模",
            "新建项目",
        ] {
            assert!(text.contains(label), "{label}: {text}");
        }
    }
}
