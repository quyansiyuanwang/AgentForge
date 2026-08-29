//! Terminal interaction layer. The TUI emits intents; application services own all mutations.

use agentforge_core::model::Target;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, widgets::Paragraph};
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SelectTargets(Vec<Target>),
    ToggleTarget(Target),
    Confirm,
    Back,
    Cancel,
    Resize { width: u16, height: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowState {
    Selecting { targets: Vec<Target> },
    Reviewing { targets: Vec<Target> },
    Confirmed { targets: Vec<Target> },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewContent {
    pub detection: Vec<String>,
    pub recommendations: Vec<String>,
    pub source_risks: Vec<String>,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewPage {
    Detection,
    Recommendations,
    Sources,
    Changes,
}

impl ReviewPage {
    const fn next(self) -> Option<Self> {
        match self {
            Self::Detection => Some(Self::Recommendations),
            Self::Recommendations => Some(Self::Sources),
            Self::Sources => Some(Self::Changes),
            Self::Changes => None,
        }
    }

    const fn previous(self) -> Option<Self> {
        match self {
            Self::Detection => None,
            Self::Recommendations => Some(Self::Detection),
            Self::Sources => Some(Self::Recommendations),
            Self::Changes => Some(Self::Sources),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewState {
    Reviewing { page: ReviewPage },
    Confirmed,
    Cancelled,
}

impl ReviewState {
    pub const fn new() -> Self {
        Self::Reviewing {
            page: ReviewPage::Detection,
        }
    }

    pub fn reduce(self, intent: Intent) -> Self {
        match (self, intent) {
            (Self::Reviewing { page }, Intent::Confirm) => match page.next() {
                Some(page) => Self::Reviewing { page },
                None => Self::Confirmed,
            },
            (Self::Reviewing { page }, Intent::Back) => Self::Reviewing {
                page: page.previous().unwrap_or(page),
            },
            (Self::Reviewing { .. }, Intent::Cancel) => Self::Cancelled,
            (state, _) => state,
        }
    }
}

impl Default for ReviewState {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowState {
    pub fn new() -> Self {
        Self::Selecting {
            targets: Vec::new(),
        }
    }

    pub fn reduce(self, intent: Intent) -> Self {
        match (self, intent) {
            (Self::Selecting { .. }, Intent::SelectTargets(targets)) => Self::Reviewing { targets },
            (Self::Selecting { targets }, Intent::ToggleTarget(target)) => Self::Selecting {
                targets: toggle_target(targets, target),
            },
            (Self::Reviewing { targets }, Intent::ToggleTarget(target)) => Self::Reviewing {
                targets: toggle_target(targets, target),
            },
            (Self::Selecting { targets }, Intent::Confirm) => Self::Reviewing { targets },
            (Self::Reviewing { targets }, Intent::Confirm) => Self::Confirmed { targets },
            (Self::Selecting { .. } | Self::Reviewing { .. }, Intent::Cancel) => Self::Cancelled,
            (state, Intent::Resize { .. }) => state,
            (state, _) => state,
        }
    }
}

impl Default for FlowState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProfile {
    pub width: u16,
    pub height: u16,
}

impl TerminalProfile {
    pub const fn compact() -> Self {
        Self {
            width: 80,
            height: 24,
        }
    }
    pub const fn standard() -> Self {
        Self {
            width: 120,
            height: 30,
        }
    }
    pub const fn supports_minimum(self) -> bool {
        self.width >= 80 && self.height >= 24
    }
}

pub fn normalize_resize(width: u16, height: u16) -> Intent {
    Intent::Resize { width, height }
}

pub fn intent_from_event(event: Event) -> Option<Intent> {
    match event {
        Event::Key(key) => match key.code {
            KeyCode::Enter => Some(Intent::Confirm),
            KeyCode::Esc => Some(Intent::Cancel),
            KeyCode::Backspace | KeyCode::Left => Some(Intent::Back),
            _ => None,
        },
        Event::Resize(width, height) => Some(normalize_resize(width, height)),
        _ => None,
    }
}

pub fn review_frame(
    content: &ReviewContent,
    state: &ReviewState,
    profile: TerminalProfile,
) -> Buffer {
    let (title, lines) = match state {
        ReviewState::Reviewing { page } => match page {
            ReviewPage::Detection => ("Detection facts", &content.detection),
            ReviewPage::Recommendations => {
                ("Recommendations and reasons", &content.recommendations)
            }
            ReviewPage::Sources => ("Remote source risks", &content.source_risks),
            ReviewPage::Changes => ("Resolved changes", &content.changes),
        },
        ReviewState::Confirmed => ("Confirmed", &content.changes),
        ReviewState::Cancelled => ("Cancelled", &content.changes),
    };
    let body = if lines.is_empty() {
        "None".to_owned()
    } else {
        lines.join("\n")
    };
    preview_frame(title, &body, profile)
}

/// Runs target selection and returns only an intent-derived state. Callers own persistence.
pub fn run_target_selection() -> io::Result<FlowState> {
    enable_raw_mode()?;
    let _terminal_guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut state = FlowState::new();
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let body = match &state {
                FlowState::Selecting { targets } => format!(
                    "Targets: {}\nPress 1-4 to select, Enter to review, Esc to cancel",
                    target_names(targets)
                ),
                FlowState::Reviewing { targets } => format!(
                    "Selected: {}\nPress Enter to confirm, Esc to cancel",
                    target_names(targets)
                ),
                FlowState::Confirmed { targets } => format!("Confirmed: {}", target_names(targets)),
                FlowState::Cancelled => "Cancelled".to_owned(),
            };
            frame.render_widget(ratatui::widgets::Paragraph::new(body), area);
        })?;
        if matches!(state, FlowState::Confirmed { .. } | FlowState::Cancelled) {
            break;
        }
        if event::poll(std::time::Duration::from_millis(100))? {
            let event = event::read()?;
            let intent = match event {
                Event::Key(key) if key.code == KeyCode::Char('1') => {
                    Some(Intent::ToggleTarget(Target::Generic))
                }
                Event::Key(key) if key.code == KeyCode::Char('2') => {
                    Some(Intent::ToggleTarget(Target::Codex))
                }
                Event::Key(key) if key.code == KeyCode::Char('3') => {
                    Some(Intent::ToggleTarget(Target::Claude))
                }
                Event::Key(key) if key.code == KeyCode::Char('4') => {
                    Some(Intent::ToggleTarget(Target::Copilot))
                }
                other => intent_from_event(other),
            };
            if let Some(intent) = intent {
                state = state.reduce(intent);
            }
        }
    }
    terminal.show_cursor()?;
    Ok(state)
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

fn target_names(targets: &[Target]) -> String {
    if targets.is_empty() {
        "none".into()
    } else {
        targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn toggle_target(mut targets: Vec<Target>, target: Target) -> Vec<Target> {
    if target == Target::Generic {
        return if targets.contains(&target) {
            Vec::new()
        } else {
            vec![target]
        };
    }
    targets.retain(|item| *item != Target::Generic);
    if let Some(index) = targets.iter().position(|item| *item == target) {
        targets.remove(index);
    } else {
        targets.push(target);
    }
    targets.sort();
    targets
}

/// Renders a deterministic preview frame for both the interactive terminal and snapshot tests.
pub fn preview_frame(title: &str, body: &str, profile: TerminalProfile) -> Buffer {
    let area = Rect::new(0, 0, profile.width, profile.height);
    let backend = TestBackend::new(profile.width, profile.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test backend is infallible");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(format!("{title}\n{body}")), area))
        .expect("test backend draw is infallible");
    terminal.backend().buffer().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_match_supported_snapshots() {
        assert!(TerminalProfile::compact().supports_minimum());
        assert!(TerminalProfile::standard().supports_minimum());
        assert!(
            !TerminalProfile {
                width: 79,
                height: 24
            }
            .supports_minimum()
        );
    }

    #[test]
    fn confirmation_is_explicit_and_cancel_is_terminal() {
        let state = FlowState::new()
            .reduce(Intent::SelectTargets(vec![Target::Codex]))
            .reduce(Intent::Confirm);
        assert_eq!(
            state,
            FlowState::Confirmed {
                targets: vec![Target::Codex]
            }
        );
        assert_eq!(
            FlowState::new().reduce(Intent::Cancel),
            FlowState::Cancelled
        );
    }

    #[test]
    fn target_toggle_supports_multi_vendor_and_generic_exclusion() {
        let state = FlowState::new()
            .reduce(Intent::ToggleTarget(Target::Codex))
            .reduce(Intent::ToggleTarget(Target::Claude))
            .reduce(Intent::Confirm)
            .reduce(Intent::Confirm);
        assert_eq!(
            state,
            FlowState::Confirmed {
                targets: vec![Target::Codex, Target::Claude]
            }
        );
        let state = FlowState::new()
            .reduce(Intent::ToggleTarget(Target::Generic))
            .reduce(Intent::ToggleTarget(Target::Codex));
        assert_eq!(
            state,
            FlowState::Selecting {
                targets: vec![Target::Codex]
            }
        );
    }

    #[test]
    fn terminal_events_only_produce_intents() {
        assert_eq!(
            intent_from_event(Event::Resize(120, 30)),
            Some(Intent::Resize {
                width: 120,
                height: 30
            })
        );
        assert_eq!(
            intent_from_event(Event::Key(crossterm::event::KeyEvent::from(KeyCode::Esc))),
            Some(Intent::Cancel)
        );
    }

    #[test]
    fn review_requires_each_page_confirmation_and_supports_back_cancel() {
        let state = ReviewState::new()
            .reduce(Intent::Confirm)
            .reduce(Intent::Confirm)
            .reduce(Intent::Back);
        assert_eq!(
            state,
            ReviewState::Reviewing {
                page: ReviewPage::Recommendations
            }
        );
        assert_eq!(
            ReviewState::new().reduce(Intent::Cancel),
            ReviewState::Cancelled
        );
        let confirmed = ReviewState::new()
            .reduce(Intent::Confirm)
            .reduce(Intent::Confirm)
            .reduce(Intent::Confirm)
            .reduce(Intent::Confirm);
        assert_eq!(confirmed, ReviewState::Confirmed);
    }

    #[test]
    fn review_frame_is_stable_at_supported_terminal_sizes() {
        let content = ReviewContent {
            detection: vec!["nextjs (0.99)".into()],
            recommendations: vec!["codex: render AGENTS.md".into()],
            source_risks: vec!["github skill: executable content".into()],
            changes: vec!["CREATE AGENTS.md".into()],
        };
        let frame = review_frame(&content, &ReviewState::new(), TerminalProfile::compact());
        assert_eq!(frame.area().width, 80);
        assert_eq!(frame.area().height, 24);
        let frame = review_frame(
            &content,
            &ReviewState::Confirmed,
            TerminalProfile::standard(),
        );
        assert_eq!(frame.area().width, 120);
        assert_eq!(frame.area().height, 30);
    }
}
