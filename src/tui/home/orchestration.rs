//! Orchestrated groups in the home view: the board and memory side panel, posting to the
//! board, and starting a group from the new-session dialog.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::*;

use super::*;
use crate::session::orchestrated_group::{
    self, Board, BoardMessage, MessageKind, OrchestrationInfo, ALL_WORKERS, SYSTEM_AUTHOR,
    USER_AUTHOR,
};
use crate::tui::components::{prefix_within_width, rendered_width, truncate_to_width};
use crate::tui::dialogs::{NewSessionData, SendMessageDialog};
use crate::tui::styles::Theme;

const REFRESH: Duration = Duration::from_secs(1);
const PANEL_MIN_WIDTH: u16 = 40;
const MAX_MESSAGES: usize = 300;
const NICK_WIDTH: usize = 12;
/// `HH:MM ` + nick + ` │ `.
const GUTTER: usize = 6 + NICK_WIDTH + 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum PanelTab {
    Board,
    Memory,
}

pub(in crate::tui) struct OrchestrationPanel {
    tab: PanelTab,
    /// Keys go to the panel instead of the session list.
    focused: bool,
    /// Selected message seq; `None` follows the newest message.
    selected: Option<u64>,
    expanded: HashSet<u64>,
    expand_all: bool,
    show_notices: bool,
    /// First visible line, kept between frames so the view does not jump.
    top: usize,
    memory_scroll: u16,
    cache: Option<PanelCache>,
}

impl OrchestrationPanel {
    fn new() -> Self {
        Self {
            tab: PanelTab::Board,
            focused: true,
            selected: None,
            expanded: HashSet::new(),
            expand_all: false,
            show_notices: true,
            top: 0,
            memory_scroll: 0,
            cache: None,
        }
    }

    /// Move the selection over chat messages; notices are not selectable. Stepping past
    /// the newest returns to following the tail.
    fn select_step(&mut self, delta: isize) {
        let Some(cache) = &self.cache else { return };
        let seqs: Vec<u64> = cache
            .messages
            .iter()
            .filter(|(_, m)| !is_notice(m))
            .map(|(s, _)| *s)
            .collect();
        let Some(&last) = seqs.last() else { return };
        let Some(current) = self.selected else {
            if delta < 0 {
                self.selected = Some(last);
            }
            return;
        };
        let idx = seqs
            .iter()
            .position(|s| *s == current)
            .unwrap_or(seqs.len() - 1);
        let next = idx as isize + delta;
        self.selected = if next >= seqs.len() as isize {
            None
        } else {
            Some(seqs[next.max(0) as usize])
        };
    }
}

fn is_notice(m: &BoardMessage) -> bool {
    m.kind == MessageKind::System || m.from == SYSTEM_AUTHOR
}

struct PanelCache {
    profile: String,
    group_path: String,
    loaded: Instant,
    messages: Vec<(u64, BoardMessage)>,
    /// Session id to a short display nick.
    nicks: HashMap<String, String>,
    /// Session id to full title, to map relay notices back to nicks.
    titles: HashMap<String, String>,
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

    /// `|` from the list: open the panel, or focus it when it is already open.
    pub(super) fn cycle_orchestration_panel(&mut self) {
        if let Some(panel) = &mut self.orchestration_panel {
            panel.focused = true;
        } else if self.selected_orchestration().is_some() {
            self.orchestration_panel = Some(OrchestrationPanel::new());
        } else {
            self.flash_status("Not an orchestrated group; start one with n and tick Orchestrate");
        }
    }

    pub(super) fn orchestration_panel_focused(&self) -> bool {
        self.orchestration_panel.as_ref().is_some_and(|p| p.focused)
    }

    /// Keys while the panel has focus. Every key is consumed so a stray letter cannot
    /// trigger a list action behind the panel.
    pub(super) fn handle_orchestration_panel_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.orchestration_panel else {
            return;
        };
        match (panel.tab, key.code) {
            (_, KeyCode::Esc) => panel.focused = false,
            (_, KeyCode::Char('|')) => self.orchestration_panel = None,
            (_, KeyCode::Tab | KeyCode::BackTab) => {
                panel.tab = match panel.tab {
                    PanelTab::Board => PanelTab::Memory,
                    PanelTab::Memory => PanelTab::Board,
                }
            }
            (_, KeyCode::Char('\\' | 'm')) => self.open_board_post_dialog(),
            (PanelTab::Board, KeyCode::Up | KeyCode::Char('k')) => panel.select_step(-1),
            (PanelTab::Board, KeyCode::Down | KeyCode::Char('j')) => panel.select_step(1),
            (PanelTab::Board, KeyCode::Home | KeyCode::Char('g')) => {
                panel.selected = panel
                    .cache
                    .as_ref()
                    .and_then(|c| c.messages.first().map(|(s, _)| *s));
            }
            (PanelTab::Board, KeyCode::End | KeyCode::Char('G')) => panel.selected = None,
            (PanelTab::Board, KeyCode::Enter | KeyCode::Char(' ')) => {
                let target = panel.selected.or_else(|| {
                    panel
                        .cache
                        .as_ref()
                        .and_then(|c| c.messages.last().map(|(s, _)| *s))
                });
                if let Some(seq) = target {
                    if !panel.expanded.remove(&seq) {
                        panel.expanded.insert(seq);
                    }
                }
            }
            (PanelTab::Board, KeyCode::Char('e')) => panel.expand_all = !panel.expand_all,
            (PanelTab::Board, KeyCode::Char('s')) => panel.show_notices = !panel.show_notices,
            (PanelTab::Memory, KeyCode::Up | KeyCode::Char('k')) => {
                panel.memory_scroll = panel.memory_scroll.saturating_sub(1)
            }
            (PanelTab::Memory, KeyCode::Down | KeyCode::Char('j')) => {
                panel.memory_scroll = panel.memory_scroll.saturating_add(1)
            }
            (PanelTab::Memory, KeyCode::Home | KeyCode::Char('g')) => panel.memory_scroll = 0,
            _ => {}
        }
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
            Some(w) => format!("#{} → {}", group.path, w.title),
            None => format!("#{} → chief", group.path),
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
            panel.selected = None;
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
            if let Some(panel) = &mut self.orchestration_panel {
                panel.focused = false;
            }
            self.render_preview(frame, area, theme);
            return;
        };
        let panel_width = (area.width * 2 / 5).max(PANEL_MIN_WIDTH);
        let [preview_area, panel_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(panel_width)]).areas(area);
        self.render_preview(frame, preview_area, theme);

        let info = group
            .orchestration
            .clone()
            .expect("filtered on orchestration");
        let stale = self
            .orchestration_panel
            .as_ref()
            .and_then(|p| p.cache.as_ref())
            .is_none_or(|c| {
                c.profile != profile || c.group_path != group.path || c.loaded.elapsed() > REFRESH
            });
        if stale {
            let nicks = self.nicks_for(&group, &info);
            let titles = nicks
                .keys()
                .filter_map(|id| Some((id.clone(), self.get_instance(id)?.title.clone())))
                .collect();
            let panel = self.orchestration_panel.as_mut().expect("checked above");
            panel.cache = Some(load_cache(&profile, &group.path, &info, nicks, titles));
        }
        let panel = self.orchestration_panel.as_mut().expect("checked above");
        render_panel(frame, panel_area, theme, panel, &group.path);
    }

    fn nicks_for(&self, group: &Group, info: &OrchestrationInfo) -> HashMap<String, String> {
        let chief = info.chief_session_id.as_deref();
        let workers: Vec<(&str, &str)> = self
            .instances()
            .filter(|i| i.group_path == group.path && Some(i.id.as_str()) != chief)
            .map(|i| (i.id.as_str(), i.title.as_str()))
            .collect();
        let mut nicks = worker_nicks(&workers);
        if let Some(chief) = chief {
            nicks.insert(chief.to_string(), "chief".into());
        }
        nicks.insert(USER_AUTHOR.into(), "you".into());
        nicks.insert(ALL_WORKERS.into(), "all".into());
        nicks
    }
}

/// Worker titles minus the leading words they all share, so `ndmi Country` and
/// `ndmi QuoteSheet` read `Country` and `QuoteSheet`.
fn worker_nicks(workers: &[(&str, &str)]) -> HashMap<String, String> {
    let words: Vec<Vec<&str>> = workers
        .iter()
        .map(|(_, t)| t.split_whitespace().collect())
        .collect();
    let shortest = words.iter().map(Vec::len).min().unwrap_or(0);
    let shared = if workers.len() < 2 {
        0
    } else {
        (0..shortest.saturating_sub(1))
            .take_while(|&i| words.iter().all(|w| w[i] == words[0][i]))
            .count()
    };
    workers
        .iter()
        .zip(&words)
        .map(|((id, _), w)| (id.to_string(), w[shared.min(w.len())..].join(" ")))
        .collect()
}

fn load_cache(
    profile: &str,
    group_path: &str,
    info: &OrchestrationInfo,
    nicks: HashMap<String, String>,
    titles: HashMap<String, String>,
) -> PanelCache {
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
        nicks,
        titles,
        memory,
        topic_files,
    }
}

fn nick_color(id: &str, theme: &Theme) -> Color {
    let palette = [
        theme.accent,
        theme.running,
        theme.waiting,
        theme.branch,
        theme.group,
        theme.favorite,
        theme.search,
        theme.sandbox,
    ];
    let hash = id
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    palette[hash % palette.len()]
}

/// `(tag, color)` for kinds worth calling out; chat-like kinds get none.
fn kind_tag(kind: MessageKind, theme: &Theme) -> Option<(&'static str, Color)> {
    match kind {
        MessageKind::Task => Some(("task", theme.accent)),
        MessageKind::Question => Some(("ask", theme.waiting)),
        MessageKind::Answer => Some(("re", theme.running)),
        MessageKind::Result => Some(("done", theme.running)),
        MessageKind::Learning => Some(("learn", theme.branch)),
        MessageKind::Status | MessageKind::Note | MessageKind::System => None,
    }
}

/// Relay notices name sessions by title and id; the panel already shows who is who.
fn tidy_system_body(body: &str) -> String {
    let first = body.split(';').next().unwrap_or(body);
    let mut out = String::with_capacity(first.len());
    let mut rest = first;
    while let Some(start) = rest.find(" (") {
        let after = &rest[start + 2..];
        let is_id = after
            .find(')')
            .is_some_and(|end| end >= 8 && after[..end].bytes().all(|b| b.is_ascii_hexdigit()));
        if is_id {
            out.push_str(&rest[..start]);
            rest = &after[after.find(')').expect("checked") + 1..];
        } else {
            out.push_str(&rest[..start + 2]);
            rest = after;
        }
    }
    out.push_str(rest);
    out.trim_start_matches("worker ").replace('\'', "")
}

/// Split a relay notice into the session it is about and a short verb.
fn system_event(body: &str) -> (String, String) {
    let tidy = tidy_system_body(body);
    for (needle, verb) in [
        (" finished its turn", "finished"),
        (" is waiting for input", "needs input"),
        (" hit an error", "errored"),
    ] {
        if let Some(i) = tidy.find(needle) {
            return (tidy[..i].to_string(), verb.to_string());
        }
    }
    (String::new(), tidy)
}

/// Greedy word wrap to `width` columns, falling back to a hard break for long words.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut rest = para.trim_end();
        if rest.is_empty() {
            out.push(String::new());
            continue;
        }
        while rendered_width(rest) > width {
            let head = prefix_within_width(rest, width);
            let cut = if rest[head.len()..].starts_with(' ') {
                head.len()
            } else {
                head.rfind(' ')
                    .filter(|&i| i > 0)
                    .unwrap_or(head.len().max(1))
            };
            out.push(rest[..cut].to_string());
            rest = rest[cut..].trim_start();
        }
        out.push(rest.to_string());
    }
    out
}

/// One message as panel lines. Collapsed messages take one line; the gutter repeats
/// as blank space under wrapped lines so bodies stay in a column.
fn message_lines(
    m: &BoardMessage,
    cache: &PanelCache,
    expanded: bool,
    selected: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let time =
        m.at.with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
    let body_width = width.saturating_sub(GUTTER).max(8);
    let dim = Style::default().fg(theme.dimmed);
    let bg = |s: Style| {
        if selected {
            s.bg(theme.selection)
        } else {
            s
        }
    };
    let nick_of = |id: &str, fallback: &str| {
        cache
            .nicks
            .get(id)
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    };

    let nick_style = Style::default().fg(nick_color(&m.from, theme)).bold();
    let nick = format!(
        "{:>w$}",
        truncate_to_width(&nick_of(&m.from, &m.from_title), NICK_WIDTH),
        w = NICK_WIDTH
    );

    let mut prefix: Vec<Span<'static>> = Vec::new();
    if let Some((tag, color)) = kind_tag(m.kind, theme) {
        prefix.push(Span::styled(
            format!("{tag} "),
            bg(Style::default().fg(color).bold()),
        ));
    }
    if let Some(to) = m.to.as_deref() {
        prefix.push(Span::styled(
            format!("@{} ", nick_of(to, to)),
            bg(Style::default().fg(nick_color(to, theme))),
        ));
    }
    let prefix_width: usize = prefix.iter().map(|s| rendered_width(&s.content)).sum();
    let body = m.body.trim().to_string();
    let body_style = Style::default().fg(theme.text);

    let gutter = |first: bool| -> Vec<Span<'static>> {
        if first {
            vec![
                Span::styled(format!("{time} "), bg(dim)),
                Span::styled(nick.clone(), bg(nick_style)),
                Span::styled(" │ ", bg(dim)),
            ]
        } else {
            vec![
                Span::styled(" ".repeat(GUTTER - 2), bg(Style::default())),
                Span::styled("│ ", bg(dim)),
            ]
        }
    };

    let first_width = body_width.saturating_sub(prefix_width).max(4);
    if !expanded {
        let first_line = body.lines().next().unwrap_or("");
        let more_lines = body.lines().count().saturating_sub(1);
        let overflow = rendered_width(first_line) > first_width || more_lines > 0;
        let marker = if more_lines > 0 {
            format!(" +{more_lines}")
        } else {
            String::new()
        };
        let room = first_width.saturating_sub(if overflow {
            rendered_width(&marker) + 1
        } else {
            0
        });
        let shown = if overflow {
            let head = prefix_within_width(first_line, room);
            format!("{}…", head.trim_end())
        } else {
            first_line.to_string()
        };
        let mut spans = gutter(true);
        spans.extend(prefix);
        spans.push(Span::styled(shown, bg(body_style)));
        if overflow && !marker.is_empty() {
            spans.push(Span::styled(marker, bg(dim)));
        }
        return vec![Line::from(spans)];
    }

    let mut lines = Vec::new();
    let mut wrapped = wrap(&body, body_width);
    // The first line shares its row with the prefix.
    if let Some(first) = wrapped.first().cloned() {
        if rendered_width(&first) > first_width {
            let mut rest = wrap(&first, first_width);
            let head = rest.remove(0);
            let tail = rest.join(" ");
            wrapped[0] = head;
            if !tail.is_empty() {
                wrapped.splice(1..1, wrap(&tail, body_width));
            }
        }
    }
    for (i, text) in wrapped.into_iter().enumerate() {
        let mut spans = gutter(i == 0);
        if i == 0 {
            spans.extend(prefix.clone());
        }
        spans.push(Span::styled(text, bg(body_style)));
        lines.push(Line::from(spans));
    }
    lines
}

fn render_panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    panel: &mut OrchestrationPanel,
    group_path: &str,
) {
    let border = if panel.focused {
        theme.accent
    } else {
        theme.border
    };
    let tab = |name: &'static str, active: bool| {
        if active {
            Span::styled(name, Style::default().fg(theme.title).bold().underlined())
        } else {
            Span::styled(name, Style::default().fg(theme.dimmed))
        }
    };
    let title = Line::from(vec![
        Span::styled(
            format!(" #{group_path} "),
            Style::default().fg(theme.title).bold(),
        ),
        Span::styled("· ", Style::default().fg(theme.dimmed)),
        tab("board", panel.tab == PanelTab::Board),
        Span::styled(" ", Style::default()),
        tab("memory", panel.tab == PanelTab::Memory),
        Span::raw(" "),
    ]);
    let hint = match (panel.focused, panel.tab) {
        (false, _) => " | focus  \\ post ",
        (true, PanelTab::Board) => {
            " ↑↓ select  ⏎ expand  e all  s notices  ⇥ memory  \\ post  esc "
        }
        (true, PanelTab::Memory) => " ↑↓ scroll  ⇥ board  \\ post  esc ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title)
        .title_bottom(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.hint),
        )));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let cache = panel.cache.as_ref().expect("loaded before render");
    match panel.tab {
        PanelTab::Board => {
            let (lines, top) = board_lines(panel, cache, inner, theme);
            frame.render_widget(Paragraph::new(lines), inner);
            panel.top = top;
        }
        PanelTab::Memory => {
            frame.render_widget(
                Paragraph::new(memory_text(cache, theme))
                    .wrap(Wrap { trim: false })
                    .scroll((panel.memory_scroll, 0)),
                inner,
            );
        }
    }
}

/// Chat order, newest at the bottom. With no selection the view follows the tail;
/// otherwise it scrolls just enough to keep the selection visible.
fn board_lines(
    panel_state: &OrchestrationPanel,
    cache: &PanelCache,
    inner: Rect,
    theme: &Theme,
) -> (Vec<Line<'static>>, usize) {
    if cache.messages.is_empty() {
        let empty = Line::styled(
            "No messages yet. Press \\ to post.",
            Style::default().fg(theme.dimmed),
        );
        return (vec![empty], 0);
    }
    let width = inner.width as usize;
    let height = inner.height as usize;
    let mut all: Vec<Line<'static>> = Vec::new();
    let mut selected_range = None;
    let mut notices: Vec<&BoardMessage> = Vec::new();
    for (seq, m) in &cache.messages {
        if is_notice(m) {
            if panel_state.show_notices {
                notices.push(m);
            }
            continue;
        }
        all.extend(notice_lines(&notices, cache, width, theme));
        notices.clear();
        let selected = panel_state.focused && panel_state.selected == Some(*seq);
        let expanded = panel_state.expand_all || panel_state.expanded.contains(seq);
        let start = all.len();
        all.extend(message_lines(m, cache, expanded, selected, width, theme));
        if selected {
            selected_range = Some((start, all.len()));
        }
    }
    all.extend(notice_lines(&notices, cache, width, theme));
    let top = match selected_range {
        None => all.len().saturating_sub(height),
        Some((start, end)) => {
            let mut top = panel_state.top.min(all.len().saturating_sub(1));
            if start < top {
                top = start;
            }
            if end > top + height {
                top = end.saturating_sub(height);
            }
            top
        }
    };
    (all.into_iter().skip(top).take(height).collect(), top)
}

/// A run of relay notices as one dim line per verb: `-!- Country, QuoteSheet finished`.
fn notice_lines(
    run: &[&BoardMessage],
    cache: &PanelCache,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(last) = run.last() else {
        return Vec::new();
    };
    let title_nick: HashMap<String, String> = cache
        .titles
        .iter()
        .filter_map(|(id, title)| Some((title.clone(), cache.nicks.get(id)?.clone())))
        .collect();
    let mut verbs: Vec<(String, Vec<String>)> = Vec::new();
    for m in run {
        let (who, verb) = system_event(&m.body);
        let who = title_nick.get(&who).cloned().unwrap_or(who);
        match verbs.iter_mut().find(|(v, _)| *v == verb) {
            Some((_, names)) if !names.contains(&who) => names.push(who),
            Some(_) => {}
            None => verbs.push((verb, vec![who])),
        }
    }
    let time = last
        .at
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string();
    let dim = Style::default().fg(theme.dimmed);
    verbs
        .into_iter()
        .map(|(verb, names)| {
            let text = format!("{} {verb}", names.join(", ")).trim().to_string();
            let room = width.saturating_sub(GUTTER);
            Line::from(vec![
                Span::styled(format!("{time} "), dim),
                Span::styled(format!("{:>w$}", "-!-", w = NICK_WIDTH), dim),
                Span::styled(" │ ", dim),
                Span::styled(truncate_to_width(&text, room), dim.italic()),
            ])
        })
        .collect()
}

fn memory_text(cache: &PanelCache, theme: &Theme) -> Text<'static> {
    let mut lines: Vec<Line> = cache
        .memory
        .lines()
        .map(|l| {
            let style = if l.starts_with('#') {
                Style::default().fg(theme.title).bold()
            } else if l.starts_with("- ") {
                Style::default().fg(theme.text)
            } else {
                Style::default().fg(theme.dimmed)
            };
            Line::styled(l.to_string(), style)
        })
        .collect();
    if !cache.topic_files.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("files: {}", cache.topic_files.join(" · ")),
            Style::default().fg(theme.hint),
        ));
    }
    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidies_relay_notices_and_shortens_nicks() {
        assert_eq!(
            tidy_system_body(
                "worker 'ndmi Country' (2100301b6bb741cc) finished its turn; check the board"
            ),
            "ndmi Country finished its turn"
        );
        assert_eq!(
            tidy_system_body("kept (not an id) text"),
            "kept (not an id) text"
        );
        let nicks = worker_nicks(&[("a", "ndmi Country"), ("b", "ndmi QuoteSheet")]);
        assert_eq!(
            (nicks["a"].as_str(), nicks["b"].as_str()),
            ("Country", "QuoteSheet")
        );
        assert_eq!(worker_nicks(&[("a", "ndmi Country")])["a"], "ndmi Country");
        assert_eq!(
            system_event("worker 'ndmi Country' (2100301b6bb741cc) is waiting for input (x)"),
            ("ndmi Country".to_string(), "needs input".to_string())
        );
    }

    #[test]
    fn wrap_breaks_on_words_and_hard_breaks_long_ones() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("one\n\ntwo", 10), ["one", "", "two"]);
    }
}
