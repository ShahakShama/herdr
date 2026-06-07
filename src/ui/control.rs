//! Keyboard-first home sidebar: the Control half (repository list) stacked above
//! the Agents half (running agents). Replaces the legacy spaces/agents sidebar
//! when in [`Mode::Home`].

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::FocusPane;
use crate::app::AppState;
use crate::terminal::TerminalRuntimeRegistry;

use super::sidebar::{expanded_sidebar_sections, render_agent_detail};

const CONTROL_HEADER_ROWS: u16 = 2;

/// Render the home sidebar: repos on top, running agents on the bottom.
pub(super) fn render_home_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    // Right-edge separator, accented while the left column has focus.
    let left_focused = matches!(app.control.focus, FocusPane::Control | FocusPane::Agents);
    let sep_style = if left_focused {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (control_area, agents_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
    render_control_half(app, frame, control_area);
    render_agent_detail(app, terminal_runtimes, frame, agents_area);
}

fn render_control_half(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let focused = app.control.focus == FocusPane::Control;

    let header_style = if focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" repos", header_style))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    if area.height <= CONTROL_HEADER_ROWS {
        return;
    }
    let body = Rect::new(
        area.x,
        area.y + CONTROL_HEADER_ROWS,
        area.width,
        area.height - CONTROL_HEADER_ROWS,
    );

    if app.control.repos.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no repos in ~/workspace",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let scroll = app.control.repo_scroll.min(app.control.repos.len().saturating_sub(1));
    for (row, (idx, repo)) in app
        .control
        .repos
        .iter()
        .enumerate()
        .skip(scroll)
        .enumerate()
    {
        if row as u16 >= body.height {
            break;
        }
        let y = body.y + row as u16;
        let selected = idx == app.control.selected_repo;
        let row_rect = Rect::new(body.x, y, body.width, 1);

        if selected {
            let bg = if focused { p.surface0 } else { p.surface_dim };
            let buf = frame.buffer_mut();
            for x in row_rect.x..row_rect.x + row_rect.width {
                buf[(x, y)].set_style(Style::default().bg(bg));
            }
        }

        let label_style = if selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let marker = if selected { "▸ " } else { "  " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(p.accent)),
                Span::styled(truncate(&repo.label, body.width.saturating_sub(3) as usize), label_style),
            ]))
            .style(if selected {
                Style::default().bg(if focused { p.surface0 } else { p.surface_dim })
            } else {
                Style::default()
            }),
            row_rect,
        );
    }

    // Action hint footer when focused and a repo is selected.
    if focused && body.height >= 2 {
        let hint_y = area.y + area.height.saturating_sub(1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " n new · r review",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, hint_y, area.width, 1),
        );
    }
}

/// Modal form for naming a new agent/worktree in the selected repository.
pub(super) fn render_create_agent_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let p = &app.palette;
    let repo_label = app
        .control
        .selected_repository()
        .map(|repo| repo.label.clone())
        .unwrap_or_else(|| "?".to_string());

    let Some(inner) = super::widgets::render_modal_shell(frame, area, 56, 7, p) else {
        return;
    };
    if inner.height < 3 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("new agent in {repo_label}"),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    let (input_text, input_style) = if app.name_input.is_empty() {
        (
            "name…".to_string(),
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )
    } else {
        (app.name_input.clone(), Style::default().fg(p.text))
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(input_text, input_style))),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "enter create · esc cancel",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        rows[2],
    );
}

fn truncate(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width - 1).collect();
    format!("{prefix}…")
}
