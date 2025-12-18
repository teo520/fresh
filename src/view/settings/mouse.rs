//! Mouse input handling for the Settings dialog.
//!
//! This module contains all mouse event handling for the settings modal,
//! including clicks, scrolling, and drag operations.

use crate::app::Editor;

use super::items::SettingControl;
use super::{FocusPanel, SettingsHit, SettingsLayout};

impl Editor {
    /// Handle mouse events when settings modal is open.
    pub(crate) fn handle_settings_mouse(
        &mut self,
        mouse_event: crossterm::event::MouseEvent,
        is_double_click: bool,
    ) -> std::io::Result<bool> {
        use crossterm::event::{MouseButton, MouseEventKind};

        // When confirm dialog or help overlay is open, consume all mouse events
        if let Some(ref state) = self.settings_state {
            if state.showing_confirm_dialog || state.showing_help {
                return Ok(false);
            }
        }

        // Handle mouse events for entry dialog (scroll support)
        if let Some(ref mut state) = self.settings_state {
            if state.showing_entry_dialog() {
                match mouse_event.kind {
                    MouseEventKind::ScrollUp => {
                        if let Some(ref mut dialog) = state.entry_dialog {
                            dialog.scroll_up();
                            return Ok(true);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(ref mut dialog) = state.entry_dialog {
                            // Use a reasonable viewport estimate (will be corrected on render)
                            dialog.scroll_down(20);
                            return Ok(true);
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // Handle scrollbar drag for entry dialog
                        return Ok(self.entry_dialog_scrollbar_drag(
                            mouse_event.column,
                            mouse_event.row,
                        ));
                    }
                    _ => {}
                }
                // Consume other events without action
                return Ok(false);
            }
        }

        let col = mouse_event.column;
        let row = mouse_event.row;

        // Track hover position and compute hover hit for visual feedback
        match mouse_event.kind {
            MouseEventKind::Moved => {
                // Compute hover hit from cached layout
                let hover_hit = self
                    .cached_layout
                    .settings_layout
                    .as_ref()
                    .and_then(|layout: &SettingsLayout| layout.hit_test(col, row));

                if let Some(ref mut state) = self.settings_state {
                    let old_hit = state.hover_hit;
                    state.hover_position = Some((col, row));
                    state.hover_hit = hover_hit;
                    // Re-render if hover target changed
                    return Ok(old_hit != hover_hit);
                }
                return Ok(false);
            }
            MouseEventKind::ScrollUp => {
                return Ok(self.settings_scroll_up(3));
            }
            MouseEventKind::ScrollDown => {
                return Ok(self.settings_scroll_down(3));
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                return Ok(self.settings_scrollbar_drag(col, row));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Handle click below
            }
            _ => return Ok(false),
        }

        // Use cached settings layout for hit testing
        let hit = self
            .cached_layout
            .settings_layout
            .as_ref()
            .and_then(|layout: &SettingsLayout| layout.hit_test(col, row));

        let Some(hit) = hit else {
            return Ok(false);
        };

        // Check if a dropdown is open and click is outside of it
        // If so, cancel the dropdown and consume the click
        if let Some(ref mut state) = self.settings_state {
            if state.is_dropdown_open() {
                let is_click_on_open_dropdown = matches!(
                    hit,
                    SettingsHit::ControlDropdown(idx) if idx == state.selected_item
                );
                if !is_click_on_open_dropdown {
                    // Click outside dropdown - cancel and restore original value
                    state.dropdown_cancel();
                    return Ok(true);
                }
            }
        }

        match hit {
            SettingsHit::Outside => {
                // Click outside modal - do nothing (only Cancel button closes)
            }
            SettingsHit::Category(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Categories;
                    state.selected_category = idx;
                    state.selected_item = 0;
                    state.scroll_panel = crate::view::ui::ScrollablePanel::new();
                    state.sub_focus = None;
                }
            }
            SettingsHit::Item(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                }
            }
            SettingsHit::ControlToggle(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                }
                self.settings_activate_current();
            }
            SettingsHit::ControlDecrement(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                }
                self.settings_decrement_current();
            }
            SettingsHit::ControlIncrement(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                }
                self.settings_increment_current();
            }
            SettingsHit::ControlDropdown(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                }
                self.settings_activate_current();
            }
            SettingsHit::ControlText(idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                    state.start_editing();
                }
            }
            SettingsHit::ControlTextListRow(idx, _row_idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;
                    state.start_editing();
                }
            }
            SettingsHit::ControlMapRow(idx, row_idx) => {
                if let Some(ref mut state) = self.settings_state {
                    state.focus_panel = FocusPanel::Settings;
                    state.selected_item = idx;

                    // Set focus on the clicked entry within the map
                    if let Some(page) = state.pages.get_mut(state.selected_category) {
                        if let Some(item) = page.items.get_mut(idx) {
                            if let SettingControl::Map(map_state) = &mut item.control {
                                // Focus the clicked row (row_idx matches entry index, or None for add-new)
                                if row_idx < map_state.entries.len() {
                                    map_state.focused_entry = Some(row_idx);
                                } else {
                                    map_state.focused_entry = None; // Add-new row
                                }
                            }
                        }
                    }
                }

                // Double-click opens the entry dialog
                if is_double_click {
                    self.settings_activate_current();
                }
            }
            SettingsHit::SaveButton => {
                self.save_settings();
            }
            SettingsHit::CancelButton => {
                if let Some(ref mut state) = self.settings_state {
                    state.visible = false;
                }
            }
            SettingsHit::ResetButton => {
                if let Some(ref mut state) = self.settings_state {
                    state.reset_current_to_default();
                }
            }
            SettingsHit::Background => {
                // Click on background inside modal - do nothing
            }
            SettingsHit::Scrollbar => {
                self.settings_scrollbar_click(row);
            }
            SettingsHit::SettingsPanel => {
                // Click on settings panel area - do nothing (scroll handled above)
            }
        }

        Ok(true)
    }

    /// Scroll settings panel up by delta items.
    fn settings_scroll_up(&mut self, delta: usize) -> bool {
        self.settings_state
            .as_mut()
            .map(|state| state.scroll_up(delta))
            .unwrap_or(false)
    }

    /// Scroll settings panel down by delta items.
    fn settings_scroll_down(&mut self, delta: usize) -> bool {
        self.settings_state
            .as_mut()
            .map(|state| state.scroll_down(delta))
            .unwrap_or(false)
    }

    /// Handle scrollbar click at the given row position.
    fn settings_scrollbar_click(&mut self, row: u16) {
        if let Some(ref scrollbar_area) = self
            .cached_layout
            .settings_layout
            .as_ref()
            .and_then(|l| l.scrollbar_area)
        {
            if scrollbar_area.height > 0 {
                let relative_y = row.saturating_sub(scrollbar_area.y);
                let ratio = relative_y as f32 / scrollbar_area.height as f32;
                if let Some(ref mut state) = self.settings_state {
                    state.scroll_to_ratio(ratio);
                }
            }
        }
    }

    /// Handle scrollbar drag at the given position.
    fn settings_scrollbar_drag(&mut self, col: u16, row: u16) -> bool {
        if let Some(ref scrollbar_area) = self
            .cached_layout
            .settings_layout
            .as_ref()
            .and_then(|l| l.scrollbar_area)
        {
            // Check if we're in or near the scrollbar area (allow some horizontal tolerance)
            let in_scrollbar_x = col >= scrollbar_area.x.saturating_sub(1)
                && col <= scrollbar_area.x + scrollbar_area.width;
            if in_scrollbar_x && scrollbar_area.height > 0 {
                let relative_y = row.saturating_sub(scrollbar_area.y);
                let ratio = relative_y as f32 / scrollbar_area.height as f32;
                if let Some(ref mut state) = self.settings_state {
                    return state.scroll_to_ratio(ratio);
                }
            }
        }
        false
    }

    /// Handle scrollbar drag for entry dialog.
    ///
    /// Computes the entry dialog scrollbar area based on the modal area
    /// and scrolls to the position based on the drag y coordinate.
    fn entry_dialog_scrollbar_drag(&mut self, col: u16, row: u16) -> bool {
        // Get the modal area from cached layout to compute entry dialog dimensions
        let modal_area = self
            .cached_layout
            .settings_layout
            .as_ref()
            .map(|l| l.modal_area)
            .unwrap_or_default();

        if modal_area.width == 0 || modal_area.height == 0 {
            return false;
        }

        // Compute entry dialog area (same logic as render_entry_dialog)
        let dialog_width = (modal_area.width * 85 / 100).min(90).max(50);
        let dialog_height = (modal_area.height * 90 / 100).max(15);
        let dialog_x = modal_area.x + (modal_area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = modal_area.y + (modal_area.height.saturating_sub(dialog_height)) / 2;

        // Inner area (content area minus borders and button row)
        let inner_y = dialog_y + 1;
        let inner_height = dialog_height.saturating_sub(5); // 1 border + 2 button/help rows + 2 padding

        // Scrollbar is at the right edge of the dialog
        let scrollbar_x = dialog_x + dialog_width - 3;

        // Check if we're in or near the scrollbar area (allow some horizontal tolerance)
        let in_scrollbar_x = col >= scrollbar_x.saturating_sub(2) && col <= dialog_x + dialog_width;

        if in_scrollbar_x && inner_height > 0 {
            let relative_y = row.saturating_sub(inner_y);
            let ratio = (relative_y as f32 / inner_height as f32).clamp(0.0, 1.0);

            if let Some(ref mut state) = self.settings_state {
                if let Some(ref mut dialog) = state.entry_dialog {
                    dialog.scroll_to_ratio(ratio);
                    return true;
                }
            }
        }

        false
    }
}
