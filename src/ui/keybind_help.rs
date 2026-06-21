use std::borrow::Cow;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::AppState;

pub(super) type HelpEntry = (String, Cow<'static, str>);
pub(super) type HelpGroup = (&'static str, Vec<HelpEntry>);

fn help_entry(key: impl Into<String>, label: &'static str) -> HelpEntry {
    (key.into(), Cow::Borrowed(label))
}

pub(super) fn keybind_help_groups(app: &AppState) -> Vec<HelpGroup> {
    let kb = &app.keybinds;
    let mut groups = vec![
        (
            "focus",
            vec![
                help_entry("alt+h / alt+l", "left column / Main pane"),
                help_entry("alt+k / alt+j", "PR pane / Agents pane"),
                help_entry("alt+1..9", "focus agent 1-9"),
            ],
        ),
        (
            "PR pane",
            vec![
                help_entry("↑↓ · j / k", "move selection"),
                help_entry("enter / space", "open person · enter a PR as reviewer"),
                help_entry("l / o", "toggle green (lgtm'd) / grey PRs"),
                help_entry("p", "open an agent by PR number"),
                help_entry("alt+w", "open the PR in Reviewable (Chrome)"),
                help_entry("q", "back to the people list"),
            ],
        ),
        (
            "agents pane",
            vec![
                help_entry("enter", "focus the selected agent in Main"),
                help_entry("n", "repo picker (q / focus-out returns)"),
                help_entry("alt+r / alt+x", "rename / kill the selected agent"),
                help_entry("alt+p", "submit a PR for the agent's branch"),
            ],
        ),
        (
            "repo / branch picker",
            vec![
                help_entry("enter / alt+enter", "agent on the branch / on a new branch"),
                help_entry("t", "terminal in the selected repo"),
                help_entry("p", "open an agent by PR number"),
                help_entry("alt+w", "open the branch's PR in Reviewable"),
                help_entry("/", "filter branches"),
            ],
        ),
        (
            "Main pane",
            vec![
                help_entry("alt+r / alt+t", "toggle the review / terminal row"),
                help_entry("alt+g", "fix CLAUDE: comments in the review diff"),
                help_entry("alt+z", "zoom the focused row"),
            ],
        ),
        (
            "commands",
            vec![
                help_entry("alt+s", "copy mode (keyboard scrollback + yank)"),
                help_entry("alt+,", "settings"),
                help_entry("alt+?", "this keybind help"),
                help_entry("alt+q", "quit herdr"),
            ],
        ),
        (
            "agent pane (reserved, sent to the agent)",
            vec![help_entry(
                "ctrl+c / d / z / l / a / e / u / w / r",
                "passed through to the focused agent",
            )],
        ),
    ];

    if !kb.custom_commands.is_empty() {
        groups.push((
            "custom",
            kb.custom_commands
                .iter()
                .map(|binding| {
                    (
                        binding.label.clone(),
                        binding
                            .description
                            .clone()
                            .map(Cow::Owned)
                            .unwrap_or(Cow::Borrowed("custom command")),
                    )
                })
                .collect(),
        ));
    }

    groups
}

pub(crate) fn keybind_help_lines(app: &AppState) -> Vec<(usize, Line<'static>)> {
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);

    let groups = keybind_help_groups(app);
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);

    let mut lines = Vec::new();

    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(vec![Span::styled(format!(" {group}"), heading_style)]),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {:<width$} ", key, width = key_width);
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(padded_key, key_style),
                    Span::styled(label.into_owned(), label_style),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }

    lines
}

pub(super) fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    super::dim_background(frame, frame.area());

    let Some(inner) = render_modal_shell(frame, frame.area(), 76, 22, &app.palette) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(frame, header_rows[0], "keybinds", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(
        Paragraph::new(" available commands and configured shortcuts")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[1],
    );

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app
            .keybind_help_max_scroll()
            .saturating_sub(app.keybind_help.scroll) as usize,
        max_offset_from_bottom: app.keybind_help_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        keybind_help_lines(app)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((app.keybind_help.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("wheel ↑↓", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("jump", Style::default().fg(app.palette.overlay0)),
            Span::styled(" pgup / pgdn ", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close", Style::default().fg(app.palette.overlay0)),
            Span::styled(" esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}
