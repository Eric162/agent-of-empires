//! Orchestrated groups in the home view: the board and memory side panel, posting to the
//! board, and starting a group from the new-session dialog.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::prelude::*;
use ratatui::widgets::*;

use super::*;
use crate::session::orchestrated_group::{
    self, Board, BoardMessage, MessageKind, OrchestrationInfo, USER_AUTHOR,
};
use crate::tui::dialogs::{NewSessionData, SendMessageDialog};
use crate::tui::styles::Theme;

const REFRESH: Duration = Duration::from_secs(2);
const PANEL_MIN_WIDTH: u16 = 36;
const MAX_MESSAGES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum PanelTab {
    Board,
    Memory,
}

pub(in crate::tui) struct OrchestrationPanel {
    tab: PanelTab,
    cache: Option<PanelCache>,
}

struct PanelCache {
    profile: String,
    group_path: String,
    loaded: Instant,
    messages: Vec<(u64, BoardMessage)>,
    memory: String,
    topic_files: Vec<String>,
}

pub(in crate::tui) struct BoardPostTarget {
    topic: String,
    to: Option<String>,
}

/// Outcome of a background `og init`, reported in the status bar.
pub(in crate::tui) type InitResult = Result<String, String>;

impl HomeView {
    /// The orchestrated group the selected row belongs to, with its profile.
    fn selected_orchestration(&self) -> Option<(String, Group)> {
        let (profile, path) = match &self.selected_session {
            Some(id) => {
                let inst = self.get_instance(id)?;
                (inst.source_profile.clone(), inst.group_path.clone())
            }
            None => (
                self.selected_group_profile
                    .clone()
                    .or_else(|| self.active_profile.clone())
                    .unwrap_or_default(),
                self.selected_group.clone()?,
            ),
        };
        let groups = self.group_trees.get(&profile)?.get_all_groups();
        orchestrated_group::find_for_path(&groups, &path)
            .cloned()
            .map(|g| (profile, g))
    }

    /// Cycle the panel: closed, board, memory, closed.
    pub(super) fn cycle_orchestration_panel(&mut self) {
        self.orchestration_panel = match self.orchestration_panel.take() {
            None if self.selected_orchestration().is_none() => {
                self.flash_status(
                    "Not an orchestrated group; start one with n and tick Orchestrate",
                );
                None
            }
            None => Some(OrchestrationPanel {
                tab: PanelTab::Board,
                cache: None,
            }),
            Some(p) if p.tab == PanelTab::Board => Some(OrchestrationPanel {
                tab: PanelTab::Memory,
                ..p
            }),
            Some(_) => None,
        };
    }

    /// Compose a board post: to the selected worker, otherwise to the chief.
    pub(super) fn open_board_post_dialog(&mut self) {
        let Some((_, group)) = self.selected_orchestration() else {
            self.flash_status("Not an orchestrated group");
            return;
        };
        let info = group
            .orchestration
            .as_ref()
            .expect("filtered on orchestration");
        let worker = self
            .selected_session
            .as_deref()
            .and_then(|id| self.get_instance(id))
            .filter(|i| info.chief_session_id.as_deref() != Some(i.id.as_str()));
        let label = match worker {
            Some(w) => format!("board → {}", w.title),
            None => format!("board → {} chief", group.path),
        };
        self.pending_board_post = Some(BoardPostTarget {
            topic: info.id.clone(),
            to: worker.map(|w| w.id.clone()),
        });
        self.send_message_dialog = Some(SendMessageDialog::new(&label));
    }

    pub(super) fn post_to_board(&mut self, target: BoardPostTarget, body: String) {
        if body.trim().is_empty() {
            return;
        }
        let msg = BoardMessage {
            kind: MessageKind::Note,
            from: USER_AUTHOR.into(),
            from_title: USER_AUTHOR.into(),
            to: target.to,
            body,
            at: chrono::Utc::now(),
        };
        match Board::open().and_then(|mut b| b.post(&target.topic, &msg)) {
            Ok(seq) => self.flash_status(format!("Posted #{seq} to the board")),
            Err(e) => self.flash_status(format!("Board post failed: {e}")),
        }
        if let Some(panel) = &mut self.orchestration_panel {
            panel.cache = None;
        }
    }

    /// Start `og init` off the UI thread; the new chief appears through the usual reload.
    pub(super) fn start_orchestrated_group(&mut self, data: NewSessionData) {
        let group = data.group.trim().trim_matches('/').to_string();
        let mut args: Vec<String> = Vec::new();
        if !data.profile.is_empty() {
            args.extend(["-p".into(), data.profile.clone()]);
        }
        args.extend(["og".into(), "init".into(), group.clone(), data.path.clone()]);
        if !data.title.trim().is_empty() {
            args.extend(["-t".into(), data.title.trim().to_string()]);
        }
        if !data.tool.is_empty() {
            args.extend(["--tool".into(), data.tool.clone()]);
        }
        if data.structured {
            args.push("--structured-view".into());
        }
        if data.yolo_mode {
            args.push("--yolo".into());
        }
        if data.worktree_enabled {
            if let Some(branch) = data.worktree_branch.filter(|b| !b.is_empty()) {
                args.extend(["--worktree".into(), branch]);
                if let Some(base) = data.base_branch.filter(|b| !b.is_empty()) {
                    args.extend(["--base-branch".into(), base]);
                }
            }
        }
        let (tx, rx) = mpsc::channel();
        self.orchestration_init_rx = Some(rx);
        self.flash_status(format!("Starting orchestrated group '{group}'…"));
        std::thread::spawn(move || {
            let result = std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    std::process::Command::new(exe)
                        .args(&args)
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| e.to_string())
                })
                .and_then(|out| {
                    if out.status.success() {
                        Ok(format!("Orchestrated group '{group}' started"))
                    } else {
                        let err = String::from_utf8_lossy(&out.stderr);
                        Err(err.lines().last().unwrap_or("og init failed").to_string())
                    }
                });
            let _ = tx.send(result);
        });
    }

    /// Report a finished background `og init`; called from the app tick.
    pub fn poll_orchestration_init(&mut self) {
        let Some(rx) = &self.orchestration_init_rx else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.orchestration_init_rx = None;
        match result {
            Ok(msg) => self.flash_status(msg),
            Err(err) => {
                self.info_dialog = Some(crate::tui::dialogs::InfoDialog::new(
                    "Orchestrated group failed",
                    &err,
                ))
            }
        }
    }

    /// Split `area` between the preview and the panel when the panel is open and the
    /// selection is in an orchestrated group; otherwise the preview gets it all.
    pub(super) fn render_preview_with_panel(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
    ) {
        let selection = self.selected_orchestration();
        let fits = area.width >= crate::tui::responsive::PREVIEW_MIN_WIDTH + PANEL_MIN_WIDTH;
        let (Some((profile, group)), true, Some(_)) =
            (selection, fits, self.orchestration_panel.as_ref())
        else {
            self.render_preview(frame, area, theme);
            return;
        };
        let panel_width = (area.width * 2 / 5).max(PANEL_MIN_WIDTH);
        let [preview_area, panel_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(panel_width)]).areas(area);
        self.render_preview(frame, preview_area, theme);

        let panel = self.orchestration_panel.as_mut().expect("checked above");
        let info = group
            .orchestration
            .as_ref()
            .expect("filtered on orchestration");
        let stale = panel.cache.as_ref().is_none_or(|c| {
            c.profile != profile || c.group_path != group.path || c.loaded.elapsed() > REFRESH
        });
        if stale {
            panel.cache = Some(load_cache(&profile, &group.path, info));
        }
        render_panel(frame, panel_area, theme, panel, &group.path);
    }
}

fn load_cache(profile: &str, group_path: &str, info: &OrchestrationInfo) -> PanelCache {
    let messages = Board::open()
        .map(|b| {
            let after = b.highest_seq(&info.id).saturating_sub(MAX_MESSAGES as u64);
            b.read(&info.id, after, None)
        })
        .unwrap_or_default();
    let dir = orchestrated_group::memory_dir(info).ok();
    let memory = dir
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("MEMORY.md")).ok())
        .unwrap_or_else(|| "(no MEMORY.md yet)".to_string());
    let mut topic_files: Vec<String> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && n != "MEMORY.md")
        .collect();
    topic_files.sort();
    PanelCache {
        profile: profile.to_string(),
        group_path: group_path.to_string(),
        loaded: Instant::now(),
        messages,
        memory,
        topic_files,
    }
}

fn kind_color(kind: MessageKind, theme: &Theme) -> Color {
    match kind {
        MessageKind::Task => theme.accent,
        MessageKind::Question => theme.waiting,
        MessageKind::Answer | MessageKind::Result => theme.running,
        MessageKind::Learning => theme.branch,
        MessageKind::System => theme.dimmed,
        MessageKind::Status | MessageKind::Note => theme.text,
    }
}

fn render_panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    panel: &OrchestrationPanel,
    group_path: &str,
) {
    let cache = panel.cache.as_ref().expect("loaded before render");
    let (title, hint) = match panel.tab {
        PanelTab::Board => (format!(" Board · {group_path} "), " | memory  \\ post "),
        PanelTab::Memory => (format!(" Memory · {group_path} "), " | close  \\ post "),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(Span::styled(title, Style::default().fg(theme.title).bold()))
        .title_bottom(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.hint),
        )));
    let text = match panel.tab {
        PanelTab::Board => board_text(cache, theme),
        PanelTab::Memory => memory_text(cache, theme),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

/// Newest first, so the latest traffic is visible without scrolling.
fn board_text(cache: &PanelCache, theme: &Theme) -> Text<'static> {
    if cache.messages.is_empty() {
        return Text::styled("(board is empty)", Style::default().fg(theme.dimmed));
    }
    let mut lines = Vec::new();
    for (seq, m) in cache.messages.iter().rev() {
        let to =
            m.to.as_deref()
                .map(|t| format!(" → {t}"))
                .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                format!("#{seq} {} ", m.kind.as_str()),
                Style::default().fg(kind_color(m.kind, theme)).bold(),
            ),
            Span::styled(
                format!("{}{to}", m.from_title),
                Style::default().fg(theme.text),
            ),
            Span::styled(
                format!("  {}", m.at.with_timezone(&chrono::Local).format("%H:%M")),
                Style::default().fg(theme.dimmed),
            ),
        ]));
        for body in m.body.lines() {
            lines.push(Line::styled(
                format!("  {body}"),
                Style::default().fg(theme.text),
            ));
        }
        lines.push(Line::default());
    }
    Text::from(lines)
}

fn memory_text(cache: &PanelCache, theme: &Theme) -> Text<'static> {
    let mut lines: Vec<Line> = cache
        .memory
        .lines()
        .map(|l| {
            let style = if l.starts_with('#') {
                Style::default().fg(theme.title).bold()
            } else {
                Style::default().fg(theme.text)
            };
            Line::styled(l.to_string(), style)
        })
        .collect();
    if !cache.topic_files.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("Topic files: {}", cache.topic_files.join(", ")),
            Style::default().fg(theme.dimmed),
        ));
    }
    Text::from(lines)
}
