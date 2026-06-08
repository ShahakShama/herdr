//! The copy-mode status bar. (The legacy prefix/navigate/resize/context-menu and
//! global-launcher overlays were removed in the keyboard-first overhaul.)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::widgets::panel_contrast_fg;
use crate::app::AppState;

fn render_bottom_bar(frame: &mut Frame, area: Rect, line: Line<'_>, bg: ratatui::style::Color) {
    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)].set_style(Style::default().bg(bg));
    }
    frame.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_copy_mode_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);
    let mode_style = Style::default()
        .fg(panel_contrast_fg(&app.palette))
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);

    let select = if app
        .copy_mode
        .is_some_and(|copy_mode| copy_mode.selection.is_some())
    {
        "selecting"
    } else {
        "select"
    };
    let line = Line::from(vec![
        Span::styled(" COPY ", mode_style),
        Span::raw(" "),
        Span::styled("h/j/k/l w/b/e { }", key),
        Span::styled(" move  ", dim),
        Span::styled("v/space", key),
        Span::styled(format!(" {select}  "), dim),
        Span::styled("y/enter", key),
        Span::styled(" copy  ", dim),
        Span::styled("q/esc", key),
        Span::styled(" exit", dim),
    ]);

    let overlay_y = area.y + area.height.saturating_sub(1);
    let overlay_area = Rect::new(area.x, overlay_y, area.width, 1);
    render_bottom_bar(frame, overlay_area, line, app.palette.panel_bg);
}
