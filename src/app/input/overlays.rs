use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
};

use crate::app::state::AppState;

impl AppState {
    fn onboarding_full_area(&self) -> Rect {
        self.view.sidebar_rect.union(self.view.terminal_area)
    }

    fn onboarding_modal_inner(&self, popup_w: u16, popup_h: u16) -> Option<Rect> {
        let area = self.onboarding_full_area();
        let popup_w = popup_w.min(area.width.saturating_sub(4));
        let popup_h = popup_h.min(area.height.saturating_sub(2));
        if popup_w < 4 || popup_h < 4 {
            return None;
        }
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);
        Some(Block::default().borders(Borders::ALL).inner(popup))
    }

    fn release_notes_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
            crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
        )
    }

    fn product_announcement_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
    }

    fn release_notes_body_rect(&self) -> Option<Rect> {
        let inner = self.release_notes_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn release_notes_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let notes = self.release_notes.as_ref()?;
        let body = self.release_notes_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::release_notes_display_lines(
            notes,
            &self.update_install_command,
            &self.palette,
        );

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(notes.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn release_notes_max_scroll(&self) -> u16 {
        self.release_notes_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn product_announcement_body_rect(&self) -> Option<Rect> {
        let inner = self.product_announcement_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn product_announcement_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let announcement = self.product_announcement.as_ref()?;
        let body = self.product_announcement_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::product_announcement_display_lines(announcement, &self.palette);

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(announcement.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn product_announcement_max_scroll(&self) -> u16 {
        self.product_announcement_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn keybind_help_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(76, 22)
    }

    fn keybind_help_body_rect(&self) -> Option<Rect> {
        let inner = self.keybind_help_modal_inner()?;
        if inner.height < 6 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn keybind_help_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let body = self.keybind_help_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let wrap_width = body.width.max(1) as usize;
        let total_rows = crate::ui::keybind_help_lines(self)
            .into_iter()
            .map(|(width, _)| width.max(1).div_ceil(wrap_width))
            .sum::<usize>();
        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom
                .saturating_sub(self.keybind_help.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn keybind_help_max_scroll(&self) -> u16 {
        self.keybind_help_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    pub(super) fn scroll_keybind_help(&mut self, delta: i16) {
        let max_scroll = self.keybind_help_max_scroll();
        let current = self.keybind_help.scroll as i16;
        self.keybind_help.scroll = current.saturating_add(delta).clamp(0, max_scroll as i16) as u16;
    }
}
