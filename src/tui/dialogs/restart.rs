//! Restart session dialog: pick profile + AI engine before respawning.
//!
//! Profile-on-restart means a heavy respawn that re-applies the new
//! profile's env (CLAUDE_CONFIG_DIR, API keys, MCP servers). Picking a
//! profile auto-populates the tool from `config.session.default_tool`,
//! mirroring `NewSessionDialog::reload_config_defaults`. A manual tool
//! override does not snap the profile, so users can keep the profile
//! and swap only the engine.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::*;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use super::DialogResult;
use crate::session::profile_config::resolve_config_or_warn;
use crate::tui::components::hover::{paint_hover_bg, HoverState};
use crate::tui::components::{profile_cycler_spans, render_text_field, tool_cycler_spans};
use crate::tui::styles::Theme;

/// Data returned when the restart dialog is submitted.
#[derive(Debug, Clone)]
pub struct RestartData {
    /// New profile (None means keep current).
    pub profile: Option<String>,
    /// New tool (None means keep current).
    pub tool: Option<String>,
    /// New extra args (None means keep current).
    pub extra_args: Option<String>,
    /// New command override (None means keep current).
    pub command_override: Option<String>,
}

pub struct RestartDialog {
    current_title: String,
    current_profile: String,
    current_tool: String,
    /// The instance's current launch command and extra args, used to decide
    /// whether the submitted values actually changed.
    current_command_override: String,
    current_extra_args: String,
    available_profiles: Vec<String>,
    available_tools: Vec<String>,
    profile_index: usize,
    tool_index: usize,
    /// 0 = profile, 1 = tool.
    focused_field: usize,
    /// Editable command override, shown in the tool-config overlay.
    command_override: Input,
    /// Editable extra args, shown in the tool-config overlay.
    extra_args: Input,
    /// True while the Ctrl+P tool-config overlay is open.
    tool_config_mode: bool,
    /// 0 = command override, 1 = extra args.
    tool_config_focused_field: usize,
    profile_selector_area: Rect,
    tool_selector_area: Rect,
    /// Which selector row the mouse is over, for the hover highlight.
    /// Visual only; never moves keyboard `focused_field`.
    hover: HoverState,
}

impl RestartDialog {
    pub fn new(
        current_title: &str,
        current_profile: &str,
        current_tool: &str,
        current_command_override: &str,
        current_extra_args: &str,
        available_profiles: Vec<String>,
        available_tools: Vec<String>,
    ) -> Self {
        let profile_index = available_profiles
            .iter()
            .position(|p| p == current_profile)
            .unwrap_or(0);
        let tool_index = available_tools
            .iter()
            .position(|t| t == current_tool)
            .unwrap_or(0);

        Self {
            current_title: current_title.to_string(),
            current_profile: current_profile.to_string(),
            current_tool: current_tool.to_string(),
            current_command_override: current_command_override.to_string(),
            current_extra_args: current_extra_args.to_string(),
            available_profiles,
            available_tools,
            profile_index,
            tool_index,
            focused_field: 0,
            command_override: Input::new(current_command_override.to_string()),
            extra_args: Input::new(current_extra_args.to_string()),
            tool_config_mode: false,
            tool_config_focused_field: 0,
            profile_selector_area: Rect::default(),
            tool_selector_area: Rect::default(),
            hover: HoverState::default(),
        }
    }

    pub fn handle_click(&mut self, col: u16, row: u16) -> Option<DialogResult<RestartData>> {
        // While the tool-config overlay is up, swallow clicks so a stray
        // click on the (now-hidden) selectors underneath can't cycle them.
        if self.tool_config_mode {
            return Some(DialogResult::Continue);
        }
        let pos = ratatui::layout::Position::from((col, row));
        if self.profile_selector_area.contains(pos) {
            self.focused_field = 0;
            if !self.available_profiles.is_empty() {
                self.profile_index = (self.profile_index + 1) % self.available_profiles.len();
                // Mirror keyboard cycling: when the profile changes,
                // re-resolve the tool default so the picker updates too.
                self.sync_tool_from_profile();
            }
            return Some(DialogResult::Continue);
        }
        if self.tool_selector_area.contains(pos) {
            self.focused_field = 1;
            if !self.available_tools.is_empty() {
                self.tool_index = (self.tool_index + 1) % self.available_tools.len();
                self.reload_tool_config();
            }
            return Some(DialogResult::Continue);
        }
        None
    }

    /// Highlight the selector row under the cursor without moving the
    /// focused field. Click commits via `handle_click`; see
    /// `ConfirmDialog::handle_hover` for the rationale (mouse drift
    /// between the user reading the dialog and hitting a keystroke must
    /// not silently shift which field that key targets). Returns `true`
    /// when the highlighted row changed.
    pub fn handle_hover(&mut self, col: u16, row: u16) -> bool {
        // The overlay covers the selectors; don't highlight rows beneath it.
        if self.tool_config_mode {
            return false;
        }
        self.hover.update(
            col,
            row,
            &[self.profile_selector_area, self.tool_selector_area],
        )
    }

    /// Re-resolve the default tool for the currently selected profile
    /// and snap `tool_index` accordingly, matching the keyboard's
    /// "cycle profile -> auto-pick its default_tool" behavior.
    fn sync_tool_from_profile(&mut self) {
        let Some(profile) = self.selected_profile().map(String::from) else {
            return;
        };
        let cfg = resolve_config_or_warn(&profile);
        if let Some(default_tool) = cfg.session.default_tool.as_ref() {
            if let Some(idx) = self.available_tools.iter().position(|t| t == default_tool) {
                self.tool_index = idx;
            }
        }
        self.reload_tool_config();
    }

    /// Returns the selected profile, or `None` if no profiles are
    /// available. The dialog refuses to submit in the `None` case; the
    /// no-profile state is only reachable via a bad config, but the
    /// panic-free path is cheap.
    fn selected_profile(&self) -> Option<&str> {
        self.available_profiles
            .get(self.profile_index)
            .map(String::as_str)
    }

    fn selected_tool(&self) -> Option<&str> {
        self.available_tools
            .get(self.tool_index)
            .map(String::as_str)
    }

    /// Profile change snaps tool to the profile's `default_tool` if that tool
    /// exists in `available_tools`; otherwise leaves tool_index where it was.
    /// Mirrors NewSessionDialog::reload_config_defaults so the behavior of
    /// "picking a profile pre-populates the AI engine" matches across the
    /// New / Rename / Restart modals.
    fn reload_tool_from_profile(&mut self) {
        let Some(profile) = self.selected_profile().map(str::to_string) else {
            return;
        };
        let config = resolve_config_or_warn(&profile);
        if let Some(ref default_tool) = config.session.default_tool {
            if let Some(idx) = self.available_tools.iter().position(|t| t == default_tool) {
                self.tool_index = idx;
            }
        }
        self.reload_tool_config();
    }

    /// Re-seed the command override and extra args inputs from the selected
    /// profile's config for the selected tool. Mirrors
    /// `NewSessionDialog::reload_tool_config` so swapping the engine in the
    /// restart modal picks up that tool's configured defaults.
    fn reload_tool_config(&mut self) {
        let Some(profile) = self.selected_profile().map(str::to_string) else {
            return;
        };
        let config = resolve_config_or_warn(&profile);
        let tool = self
            .selected_tool()
            .or_else(|| self.available_tools.first().map(String::as_str))
            .unwrap_or("claude")
            .to_string();
        self.extra_args = Input::new(
            config
                .session
                .agent_extra_args
                .get(&tool)
                .cloned()
                .unwrap_or_default(),
        );
        self.command_override = Input::new(config.session.resolve_tool_command(&tool));
    }

    fn next_field(&mut self) {
        self.focused_field = (self.focused_field + 1) % 2;
    }

    fn prev_field(&mut self) {
        self.focused_field = if self.focused_field == 0 { 1 } else { 0 };
    }

    fn is_profile_field(&self) -> bool {
        self.focused_field == 0
    }

    fn is_tool_field(&self) -> bool {
        self.focused_field == 1
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogResult<RestartData> {
        if self.tool_config_mode {
            return self.handle_tool_config_key(key);
        }

        // Ctrl+P opens the tool-config overlay (command override + extra
        // args), but only when the tool field is focused, mirroring the
        // new-session dialog's "(Ctrl + P to configure)" affordance.
        if key.code == KeyCode::Char('p')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.is_tool_field()
        {
            self.tool_config_mode = true;
            self.tool_config_focused_field = 0;
            return DialogResult::Continue;
        }

        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Enter => {
                let Some(new_profile) = self.selected_profile().map(str::to_string) else {
                    // No profiles available; refuse submit. Caller decides
                    // whether to keep the dialog open or close it.
                    return DialogResult::Continue;
                };
                let new_tool = self.selected_tool().map(str::to_string);
                let profile = if new_profile == self.current_profile {
                    None
                } else {
                    Some(new_profile)
                };
                let tool = match new_tool {
                    Some(t) if t == self.current_tool => None,
                    other => other,
                };
                let extra_args = {
                    let value = self.extra_args.value().trim().to_string();
                    if value == self.current_extra_args.trim() {
                        None
                    } else {
                        Some(value)
                    }
                };
                let command_override = {
                    let value = self.command_override.value().trim().to_string();
                    if value == self.current_command_override.trim() {
                        None
                    } else {
                        Some(value)
                    }
                };
                DialogResult::Submit(RestartData {
                    profile,
                    tool,
                    extra_args,
                    command_override,
                })
            }
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.prev_field();
                } else {
                    self.next_field();
                }
                DialogResult::Continue
            }
            KeyCode::Down => {
                self.next_field();
                DialogResult::Continue
            }
            KeyCode::Up => {
                self.prev_field();
                DialogResult::Continue
            }
            KeyCode::Left if self.is_profile_field() => {
                if self.available_profiles.is_empty() {
                    return DialogResult::Continue;
                }
                self.profile_index = if self.profile_index == 0 {
                    self.available_profiles.len() - 1
                } else {
                    self.profile_index - 1
                };
                self.reload_tool_from_profile();
                DialogResult::Continue
            }
            KeyCode::Right | KeyCode::Char(' ') if self.is_profile_field() => {
                if self.available_profiles.is_empty() {
                    return DialogResult::Continue;
                }
                self.profile_index = (self.profile_index + 1) % self.available_profiles.len();
                self.reload_tool_from_profile();
                DialogResult::Continue
            }
            KeyCode::Left if self.is_tool_field() => {
                if self.available_tools.is_empty() {
                    return DialogResult::Continue;
                }
                self.tool_index = if self.tool_index == 0 {
                    self.available_tools.len() - 1
                } else {
                    self.tool_index - 1
                };
                self.reload_tool_config();
                DialogResult::Continue
            }
            KeyCode::Right | KeyCode::Char(' ') if self.is_tool_field() => {
                if self.available_tools.is_empty() {
                    return DialogResult::Continue;
                }
                self.tool_index = (self.tool_index + 1) % self.available_tools.len();
                self.reload_tool_config();
                DialogResult::Continue
            }
            _ => DialogResult::Continue,
        }
    }

    /// Handle key events while the tool-config overlay is open. Mirrors
    /// `NewSessionDialog::handle_tool_config_key`: field 0 is the command
    /// override, field 1 is extra args; Enter/Esc close the overlay (they
    /// never submit or cancel the parent dialog).
    fn handle_tool_config_key(&mut self, key: KeyEvent) -> DialogResult<RestartData> {
        const TOOL_CMD: usize = 0;
        const TOOL_ARGS: usize = 1;
        const TOOL_MAX: usize = 2;

        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.tool_config_mode = false;
                DialogResult::Continue
            }
            KeyCode::Tab | KeyCode::Down => {
                self.tool_config_focused_field = (self.tool_config_focused_field + 1) % TOOL_MAX;
                DialogResult::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.tool_config_focused_field = if self.tool_config_focused_field == 0 {
                    TOOL_MAX - 1
                } else {
                    self.tool_config_focused_field - 1
                };
                DialogResult::Continue
            }
            _ => {
                match self.tool_config_focused_field {
                    TOOL_CMD => {
                        self.command_override
                            .handle_event(&crossterm::event::Event::Key(key));
                    }
                    TOOL_ARGS => {
                        self.extra_args
                            .handle_event(&crossterm::event::Event::Key(key));
                    }
                    _ => {}
                }
                DialogResult::Continue
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Wide enough that the Tool row's "(configured)  Ctrl+P: edit" suffix
        // isn't clipped (the cycler + suffix run past the old 54-col width).
        let dialog_area = super::centered_rect(area, 64, 14);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent))
            .title(" Restart Session ")
            .title_style(Style::default().fg(theme.title).bold());

        let inner = block.inner(dialog_area);
        frame.render_widget(block, dialog_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(1), // Title row
                Constraint::Length(1), // Current profile
                Constraint::Length(1), // Current tool
                Constraint::Length(1), // Spacer
                Constraint::Length(1), // Profile selector
                Constraint::Length(1), // Tool selector
                Constraint::Length(1), // Spacer
                Constraint::Min(1),    // Hint
            ])
            .split(inner);

        let title_line = Line::from(vec![
            Span::styled("Session: ", Style::default().fg(theme.dimmed)),
            Span::styled(&self.current_title, Style::default().fg(theme.text)),
        ]);
        frame.render_widget(Paragraph::new(title_line), chunks[0]);

        let current_profile_line = Line::from(vec![
            Span::styled("Current profile: ", Style::default().fg(theme.dimmed)),
            Span::styled(&self.current_profile, Style::default().fg(theme.text)),
        ]);
        frame.render_widget(Paragraph::new(current_profile_line), chunks[1]);

        let current_tool_line = Line::from(vec![
            Span::styled("Current tool:    ", Style::default().fg(theme.dimmed)),
            Span::styled(&self.current_tool, Style::default().fg(theme.text)),
        ]);
        frame.render_widget(Paragraph::new(current_tool_line), chunks[2]);

        self.render_profile_selector(frame, chunks[4], theme);
        self.profile_selector_area = chunks[4];
        self.render_tool_selector(frame, chunks[5], theme);
        self.tool_selector_area = chunks[5];
        self.render_hints(frame, chunks[7], theme);

        if let Some(rect) = self
            .hover
            .current_in(&[self.profile_selector_area, self.tool_selector_area])
        {
            paint_hover_bg(frame, rect, theme.selection);
        }

        if self.tool_config_mode {
            self.render_tool_config(frame, area, theme);
        }
    }

    /// Tool-config overlay (command override + extra args), reusing the same
    /// layout and `render_text_field` helper as the new-session dialog.
    fn render_tool_config(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let dialog_width: u16 = 72;
        let constraints = [
            Constraint::Length(2), // Command override
            Constraint::Length(2), // Extra args
            Constraint::Min(1),    // Hints
        ];
        let fields_height: u16 = 2 + 2 + 1;
        let dialog_height = fields_height + 4;

        let selected_tool = self
            .available_tools
            .get(self.tool_index)
            .or_else(|| self.available_tools.first())
            .map(String::as_str)
            .unwrap_or("claude");
        let title = format!(" Tool Configuration: {} ", selected_tool);

        let dialog_area = super::centered_rect(area, dialog_width, dialog_height);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent))
            .title(title)
            .title_style(Style::default().fg(theme.title).bold());

        let inner = block.inner(dialog_area);
        frame.render_widget(block, dialog_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(constraints)
            .split(inner);

        let cmd_placeholder = if self.tool_config_focused_field == 0 {
            Some("(replaces default binary)")
        } else if self.command_override.value().is_empty() {
            Some("(default)")
        } else {
            None
        };
        render_text_field(
            frame,
            chunks[0],
            "Command:",
            &self.command_override,
            self.tool_config_focused_field == 0,
            cmd_placeholder,
            theme,
        );

        let args_placeholder = if self.tool_config_focused_field == 1 {
            Some("(e.g. --port 8080)")
        } else if self.extra_args.value().is_empty() {
            Some("(none)")
        } else {
            None
        };
        render_text_field(
            frame,
            chunks[1],
            "Extra Args:",
            &self.extra_args,
            self.tool_config_focused_field == 1,
            args_placeholder,
            theme,
        );

        let hint_spans = vec![
            Span::styled("Tab", Style::default().fg(theme.hint)),
            Span::raw(" next  "),
            Span::styled("Enter", Style::default().fg(theme.hint)),
            Span::raw(" done  "),
            Span::styled("Esc", Style::default().fg(theme.hint)),
            Span::raw(" back"),
        ];
        frame.render_widget(Paragraph::new(Line::from(hint_spans)), chunks[2]);
    }

    /// Profile picker, rendered via the shared `profile_cycler_spans` so the
    /// New and Restart modals stay visually identical.
    fn render_profile_selector(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let value = self
            .available_profiles
            .get(self.profile_index)
            .map(String::as_str)
            .unwrap_or("(none)");
        let spans = profile_cycler_spans(
            "Profile:",
            value,
            self.available_profiles.len(),
            self.is_profile_field(),
            theme,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// AI-engine picker, rendered via the shared `tool_cycler_spans` so the
    /// label reads "Tool:" and the cycler matches the New dialog exactly. The
    /// Restart dialog appends the same "(configured)" summary and Ctrl+P hint
    /// the New dialog does, so the tool-config overlay is discoverable inline.
    fn render_tool_selector(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let value = self
            .available_tools
            .get(self.tool_index)
            .map(String::as_str)
            .unwrap_or("(none)");
        let mut spans = tool_cycler_spans(
            "Tool:",
            value,
            self.tool_index,
            self.available_tools.len(),
            self.is_tool_field(),
            theme,
        );
        let has_config =
            !self.extra_args.value().is_empty() || !self.command_override.value().is_empty();
        if has_config {
            spans.push(Span::styled(
                "  (configured)",
                Style::default().fg(theme.dimmed),
            ));
        }
        if self.is_tool_field() {
            spans.push(Span::styled(
                if has_config {
                    "  Ctrl+P: edit"
                } else {
                    "  (Ctrl+P to configure)"
                },
                Style::default().fg(theme.dimmed),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_hints(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Ctrl+P is surfaced inline next to the Tool row (see
        // `render_tool_selector`), so it stays out of this footer.
        let hint = Line::from(vec![
            Span::styled("Tab", Style::default().fg(theme.hint)),
            Span::raw(" switch  "),
            Span::styled("← →", Style::default().fg(theme.hint)),
            Span::raw(" cycle  "),
            Span::styled("Enter", Style::default().fg(theme.hint)),
            Span::raw(" restart  "),
            Span::styled("Esc", Style::default().fg(theme.hint)),
            Span::raw(" cancel"),
        ]);
        frame.render_widget(Paragraph::new(hint), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn profiles() -> Vec<String> {
        vec![
            "default".to_string(),
            "work".to_string(),
            "personal".to_string(),
        ]
    }

    fn tools() -> Vec<String> {
        vec![
            "claude".to_string(),
            "codex".to_string(),
            "settl".to_string(),
        ]
    }

    /// Build a dialog with no pre-existing command override or extra args,
    /// matching the common "fresh restart" case.
    fn dialog(current_profile: &str, current_tool: &str) -> RestartDialog {
        RestartDialog::new(
            "S",
            current_profile,
            current_tool,
            "",
            "",
            profiles(),
            tools(),
        )
    }

    #[test]
    fn test_new_seeds_indices_from_current() {
        let d = RestartDialog::new("My Sess", "work", "codex", "", "", profiles(), tools());
        assert_eq!(d.profile_index, 1);
        assert_eq!(d.tool_index, 1);
        assert_eq!(d.focused_field, 0);
    }

    #[test]
    fn test_new_falls_back_when_current_not_in_list() {
        let d = RestartDialog::new("S", "ghost", "ghost-tool", "", "", profiles(), tools());
        assert_eq!(d.profile_index, 0);
        assert_eq!(d.tool_index, 0);
    }

    #[test]
    fn test_new_seeds_command_and_args_inputs() {
        let d = RestartDialog::new(
            "S",
            "default",
            "claude",
            "claude-wrapper",
            "--foo bar",
            profiles(),
            tools(),
        );
        assert_eq!(d.command_override.value(), "claude-wrapper");
        assert_eq!(d.extra_args.value(), "--foo bar");
        assert!(!d.tool_config_mode);
    }

    #[test]
    fn test_esc_cancels() {
        let mut d = dialog("default", "claude");
        assert!(matches!(
            d.handle_key(key(KeyCode::Esc)),
            DialogResult::Cancel
        ));
    }

    #[test]
    fn test_enter_with_no_changes_returns_none_for_all() {
        let mut d = dialog("default", "claude");
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.profile, None);
                assert_eq!(data.tool, None);
                assert_eq!(data.extra_args, None);
                assert_eq!(data.command_override, None);
            }
            _ => panic!("Expected Submit"),
        }
    }

    #[test]
    fn test_tab_cycles_focus() {
        let mut d = dialog("default", "claude");
        assert_eq!(d.focused_field, 0);
        d.handle_key(key(KeyCode::Tab));
        assert_eq!(d.focused_field, 1);
        d.handle_key(key(KeyCode::Tab));
        assert_eq!(d.focused_field, 0);
    }

    #[test]
    fn test_shift_tab_cycles_focus_backwards() {
        let mut d = dialog("default", "claude");
        d.handle_key(shift_key(KeyCode::Tab));
        assert_eq!(d.focused_field, 1);
        d.handle_key(shift_key(KeyCode::Tab));
        assert_eq!(d.focused_field, 0);
    }

    #[test]
    fn test_right_cycles_profile_when_profile_focused() {
        let mut d = dialog("default", "claude");
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.profile_index, 1);
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.profile_index, 2);
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.profile_index, 0); // wrap
    }

    #[test]
    fn test_left_cycles_profile_backwards_when_profile_focused() {
        let mut d = dialog("default", "claude");
        d.handle_key(key(KeyCode::Left));
        assert_eq!(d.profile_index, 2); // wrap to end
        d.handle_key(key(KeyCode::Left));
        assert_eq!(d.profile_index, 1);
    }

    #[test]
    fn test_space_also_cycles_profile_forward() {
        let mut d = dialog("default", "claude");
        d.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(d.profile_index, 1);
    }

    #[test]
    fn test_arrows_cycle_tool_when_tool_focused() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.tool_index, 1);
        d.handle_key(key(KeyCode::Left));
        assert_eq!(d.tool_index, 0);
        d.handle_key(key(KeyCode::Left));
        assert_eq!(d.tool_index, 2); // wrap
    }

    #[test]
    fn test_profile_change_submits_some() {
        let mut d = dialog("default", "claude");
        d.handle_key(key(KeyCode::Right)); // profile -> work
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.profile, Some("work".to_string()));
            }
            _ => panic!("Expected Submit"),
        }
    }

    #[test]
    fn test_tool_only_change_submits_tool_some_profile_none() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(key(KeyCode::Right)); // tool -> codex
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.profile, None);
                assert_eq!(data.tool, Some("codex".to_string()));
            }
            _ => panic!("Expected Submit"),
        }
    }

    #[test]
    fn test_tool_override_does_not_snap_profile() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.profile_index, 0); // profile unchanged
    }

    #[test]
    fn test_unknown_key_is_continue() {
        let mut d = dialog("default", "claude");
        assert!(matches!(
            d.handle_key(key(KeyCode::Char('x'))),
            DialogResult::Continue
        ));
    }

    #[test]
    fn hover_highlights_selector_without_moving_focus() {
        // Stage selector rects manually; the real ones come from render().
        let mut d = dialog("default", "claude");
        d.profile_selector_area = Rect::new(2, 4, 50, 1);
        d.tool_selector_area = Rect::new(2, 5, 50, 1);
        assert_eq!(d.focused_field, 0);

        // Over the tool row: highlight it, focus unchanged.
        assert!(d.handle_hover(5, 5));
        assert_eq!(d.hover.current(), Some(d.tool_selector_area));
        assert_eq!(d.focused_field, 0, "hover must not move the focused field");

        // Off both rows clears the highlight.
        assert!(d.handle_hover(99, 99));
        assert_eq!(d.hover.current(), None);
    }

    #[test]
    fn test_enter_with_empty_profiles_does_not_panic() {
        // Pathological config (empty profiles list); Enter must not
        // index-panic. Dialog refuses to submit so the caller decides
        // what to do.
        let mut d = RestartDialog::new("S", "default", "claude", "", "", vec![], tools());
        let result = d.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, DialogResult::Continue));
    }

    #[test]
    fn test_ctrl_p_on_tool_field_opens_tool_config() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1; // tool field
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        assert!(d.tool_config_mode);
        assert_eq!(d.tool_config_focused_field, 0);
    }

    #[test]
    fn test_ctrl_p_on_profile_field_does_nothing() {
        let mut d = dialog("default", "claude");
        assert_eq!(d.focused_field, 0); // profile field
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        assert!(!d.tool_config_mode);
    }

    #[test]
    fn test_tool_config_typing_updates_extra_args() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        // field 0 is command; move to extra args (field 1).
        d.handle_key(key(KeyCode::Tab));
        assert_eq!(d.tool_config_focused_field, 1);
        d.handle_key(key(KeyCode::Char('-')));
        d.handle_key(key(KeyCode::Char('x')));
        assert_eq!(d.extra_args.value(), "-x");
    }

    #[test]
    fn test_tool_config_typing_updates_command_override() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        assert_eq!(d.tool_config_focused_field, 0); // command field
        d.handle_key(key(KeyCode::Char('z')));
        assert_eq!(d.command_override.value(), "z");
    }

    #[test]
    fn test_tool_config_tab_wraps_fields() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        assert_eq!(d.tool_config_focused_field, 0);
        d.handle_key(key(KeyCode::Tab));
        assert_eq!(d.tool_config_focused_field, 1);
        d.handle_key(key(KeyCode::Tab));
        assert_eq!(d.tool_config_focused_field, 0); // wrap
    }

    #[test]
    fn test_tool_config_esc_exits_overlay_without_cancelling() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        assert!(d.tool_config_mode);
        let result = d.handle_key(key(KeyCode::Esc));
        assert!(matches!(result, DialogResult::Continue));
        assert!(!d.tool_config_mode);
    }

    #[test]
    fn test_tool_config_enter_exits_overlay_without_submitting() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        let result = d.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, DialogResult::Continue));
        assert!(!d.tool_config_mode);
    }

    #[test]
    fn test_submit_returns_changed_extra_args() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        d.handle_key(key(KeyCode::Tab)); // -> extra args
        d.handle_key(key(KeyCode::Char('-')));
        d.handle_key(key(KeyCode::Char('v')));
        d.handle_key(key(KeyCode::Enter)); // exit overlay
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.extra_args, Some("-v".to_string()));
                assert_eq!(data.command_override, None);
            }
            _ => panic!("Expected Submit"),
        }
    }

    #[test]
    fn test_submit_returns_changed_command_override() {
        let mut d = dialog("default", "claude");
        d.focused_field = 1;
        d.handle_key(ctrl_key(KeyCode::Char('p')));
        d.handle_key(key(KeyCode::Char('w'))); // command field
        d.handle_key(key(KeyCode::Enter)); // exit overlay
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.command_override, Some("w".to_string()));
                assert_eq!(data.extra_args, None);
            }
            _ => panic!("Expected Submit"),
        }
    }

    #[test]
    fn test_submit_unchanged_command_and_args_returns_none() {
        // Seed with existing values; submitting without editing them yields
        // None so the caller leaves the instance untouched.
        let mut d = RestartDialog::new(
            "S",
            "default",
            "claude",
            "claude-wrapper",
            "--foo",
            profiles(),
            tools(),
        );
        match d.handle_key(key(KeyCode::Enter)) {
            DialogResult::Submit(data) => {
                assert_eq!(data.command_override, None);
                assert_eq!(data.extra_args, None);
            }
            _ => panic!("Expected Submit"),
        }
    }
}
