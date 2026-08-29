//! Terminal interaction layer. The TUI emits intents; application services own all mutations.

use agentforge_core::model::Target;
use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, widgets::Paragraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SelectTargets(Vec<Target>),
    Confirm,
    Cancel,
    Resize { width: u16, height: u16 },
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
}
