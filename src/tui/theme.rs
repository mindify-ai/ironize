//! Colors and glyphs.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(167, 139, 250);
pub const ACCENT_STRONG: Color = Color::Rgb(139, 92, 246);
pub const TEXT: Color = Color::Rgb(228, 228, 236);
pub const DIM: Color = Color::Rgb(130, 130, 146);
pub const FAINT: Color = Color::Rgb(84, 84, 98);
pub const SUCCESS: Color = Color::Rgb(74, 222, 128);
pub const ERROR: Color = Color::Rgb(248, 113, 113);
pub const WARN: Color = Color::Rgb(251, 191, 36);
pub const CODE: Color = Color::Rgb(125, 211, 252);
pub const USER_BG: Color = Color::Rgb(38, 38, 50);
pub const ADDED_BG: Color = Color::Rgb(22, 58, 36);
pub const REMOVED_BG: Color = Color::Rgb(72, 28, 34);
pub const SELECTED_BG: Color = Color::Rgb(48, 42, 76);
pub const SELECTION_BG: Color = Color::Rgb(76, 64, 140);

pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const BULLET: &str = "● ";

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn faint() -> Style {
    Style::default().fg(FAINT)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

pub fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}
