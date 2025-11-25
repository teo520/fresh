//! Tab bar rendering for multiple buffers

use crate::editor::BufferMetadata;
use crate::event::BufferId;
use crate::state::EditorState;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

/// Renders the tab bar showing open buffers
pub struct TabsRenderer;

/// Compute a scroll offset that keeps the active tab fully visible.
/// `tab_widths` should include separators; `active_idx` refers to the tab index (not counting separators).
pub fn compute_tab_scroll_offset(
    tab_widths: &[usize],
    active_idx: usize,
    max_width: usize,
    current_offset: usize,
    padding_between_tabs: usize,
) -> usize {
    if tab_widths.is_empty() || max_width == 0 {
        return 0;
    }

    let total_width: usize = tab_widths.iter().sum::<usize>()
        + padding_between_tabs.saturating_mul(tab_widths.len().saturating_sub(1));
    let mut tab_start = 0usize;
    let mut tab_end = 0usize;

    // Walk through widths to locate active tab boundaries.
    let mut tab_counter = 0usize;
    for w in tab_widths {
        let next = tab_start + *w;
        if tab_counter == active_idx {
            tab_end = next;
            break;
        }
        tab_start = next + padding_between_tabs;
        tab_counter += 1;
    }

    // If we didn't find the tab, keep current offset.
    if tab_end == 0 {
        return current_offset.min(total_width.saturating_sub(max_width));
    }

    // Basic rule: stick the active tab into view, prefer left-aligned start unless it overflows.
    let mut offset = tab_start;
    if tab_end.saturating_sub(offset) > max_width {
        offset = tab_end.saturating_sub(max_width);
    }

    offset.min(total_width.saturating_sub(max_width))
}

#[cfg(test)]
mod tests {
    use super::compute_tab_scroll_offset;

    #[test]
    fn offset_clamped_to_zero_when_active_first() {
        let widths = vec![5, 5, 5]; // tab widths
        let offset = compute_tab_scroll_offset(&widths, 0, 6, 10, 1);
        assert_eq!(offset, 0);
    }

    #[test]
    fn offset_moves_to_show_active_tab() {
        let widths = vec![5, 8, 6]; // active is the middle tab (index 1)
        let offset = compute_tab_scroll_offset(&widths, 1, 6, 0, 1);
        // Active tab width 8 cannot fully fit into width 6; expect it to right-align within view.
        assert_eq!(offset, 8);
    }

    #[test]
    fn offset_respects_total_width_bounds() {
        let widths = vec![3, 3, 3, 3];
        let offset = compute_tab_scroll_offset(&widths, 3, 4, 100, 1);
        let total: usize = widths.iter().sum();
        let total_with_padding = total + 3; // three gaps of width 1
        assert!(offset <= total_with_padding.saturating_sub(4));
    }
}

impl TabsRenderer {
    /// Render the tab bar for a specific split showing only its open buffers
    ///
    /// # Arguments
    /// * `frame` - The ratatui frame to render to
    /// * `area` - The rectangular area to render the tabs in
    /// * `split_buffers` - List of buffer IDs open in this split (in order)
    /// * `buffers` - All open buffers (for accessing state/metadata)
    /// * `buffer_metadata` - Metadata for buffers (contains display names for virtual buffers)
    /// * `active_buffer` - The currently active buffer ID for this split
    /// * `theme` - The active theme for colors
    /// * `is_active_split` - Whether this split is the active one
    pub fn render_for_split(
        frame: &mut Frame,
        area: Rect,
        split_buffers: &[BufferId],
        buffers: &HashMap<BufferId, EditorState>,
        buffer_metadata: &HashMap<BufferId, BufferMetadata>,
        active_buffer: BufferId,
        theme: &crate::theme::Theme,
        is_active_split: bool,
        tab_scroll_offset: usize,
    ) {
        const SCROLL_INDICATOR_LEFT: &str = "<";
        const SCROLL_INDICATOR_RIGHT: &str = ">";
        const SCROLL_INDICATOR_WIDTH: usize = 1; // Width of "<" or ">"

        let mut all_tab_spans: Vec<(Span, usize)> = Vec::new(); // Store (Span, display_width)
        let mut tab_ranges: Vec<(usize, usize)> = Vec::new(); // (start, end) positions for each tab (content only)

        // First, build all spans and calculate their display widths
        for (idx, id) in split_buffers.iter().enumerate() {
            let Some(state) = buffers.get(id) else {
                continue;
            };

            let name = state
                .buffer
                .file_path()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .or_else(|| buffer_metadata.get(id).map(|m| m.display_name.as_str()))
                .unwrap_or("[No Name]");

            let modified = if state.buffer.is_modified() { "*" } else { "" };
            // Include close button (×) on each tab
            let tab_text = format!(" {name}{modified} × ");
            let display_width = tab_text.chars().count();

            let is_active = *id == active_buffer;

            let style = if is_active {
                if is_active_split {
                    Style::default()
                        .fg(theme.tab_active_fg)
                        .bg(theme.tab_active_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme.tab_active_fg)
                        .bg(theme.tab_inactive_bg)
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default()
                    .fg(theme.tab_inactive_fg)
                    .bg(theme.tab_inactive_bg)
            };

            let start_pos = all_tab_spans.iter().map(|(_, w)| w).sum();
            let end_pos = start_pos + display_width;
            tab_ranges.push((start_pos, end_pos));

            all_tab_spans.push((Span::styled(tab_text, style), display_width));

            // Add a small separator between tabs if it's not the last tab
            if idx < split_buffers.len() - 1 {
                all_tab_spans.push((
                    Span::styled(" ", Style::default().bg(theme.tab_separator_bg)),
                    1,
                ));
            }
        }

        let mut current_spans: Vec<Span> = Vec::new();
        let max_width = area.width as usize;

        let total_width: usize = all_tab_spans.iter().map(|(_, w)| w).sum();
        let active_tab_idx = split_buffers.iter().position(|id| *id == active_buffer);

        let mut tab_widths: Vec<usize> = Vec::new();
        for (start, end) in &tab_ranges {
            tab_widths.push(end.saturating_sub(*start));
        }

        let mut offset = tab_scroll_offset.min(total_width.saturating_sub(max_width));
        if let Some(active_idx) = active_tab_idx {
            offset = compute_tab_scroll_offset(
                &tab_widths,
                active_idx,
                max_width,
                tab_scroll_offset,
                1, // separator width between tabs
            );
        }

        // Indicators reserve space; adjust once so the active tab still fits.
        let mut show_left = offset > 0;
        let mut show_right = total_width.saturating_sub(offset) > max_width;
        let mut available = max_width
            .saturating_sub((show_left as usize + show_right as usize) * SCROLL_INDICATOR_WIDTH);

        if let Some(active_idx) = active_tab_idx {
            let (start, end) = tab_ranges[active_idx];
            let active_width = end.saturating_sub(start);
            if start == 0 && active_width >= max_width {
                show_left = false;
                show_right = false;
                available = max_width;
            }

            if end.saturating_sub(offset) > available {
                offset = end.saturating_sub(available);
                offset = offset.min(total_width.saturating_sub(available));
                show_left = offset > 0;
                show_right = total_width.saturating_sub(offset) > available;
                available = max_width.saturating_sub(
                    (show_left as usize + show_right as usize) * SCROLL_INDICATOR_WIDTH,
                );
            }
            if start < offset {
                offset = start;
                show_left = offset > 0;
                show_right = total_width.saturating_sub(offset) > available;
            }
        }

        let mut rendered_width = 0;
        let mut skip_chars_count = offset;

        if show_left {
            current_spans.push(Span::styled(
                SCROLL_INDICATOR_LEFT,
                Style::default().bg(theme.tab_separator_bg),
            ));
            rendered_width += SCROLL_INDICATOR_WIDTH;
        }

        for (mut span, width) in all_tab_spans.into_iter() {
            if skip_chars_count >= width {
                skip_chars_count -= width;
                continue;
            }

            let visible_chars_in_span = width - skip_chars_count;
            if rendered_width + visible_chars_in_span
                > max_width.saturating_sub(if show_right {
                    SCROLL_INDICATOR_WIDTH
                } else {
                    0
                })
            {
                let remaining_width =
                    max_width
                        .saturating_sub(rendered_width)
                        .saturating_sub(if show_right {
                            SCROLL_INDICATOR_WIDTH
                        } else {
                            0
                        });
                let truncated_content = span
                    .content
                    .chars()
                    .skip(skip_chars_count)
                    .take(remaining_width)
                    .collect::<String>();
                span.content = std::borrow::Cow::Owned(truncated_content);
                current_spans.push(span);
                rendered_width += remaining_width;
                break;
            } else {
                let visible_content = span
                    .content
                    .chars()
                    .skip(skip_chars_count)
                    .collect::<String>();
                span.content = std::borrow::Cow::Owned(visible_content);
                current_spans.push(span);
                rendered_width += visible_chars_in_span;
                skip_chars_count = 0;
            }
        }

        if show_right && rendered_width < max_width {
            current_spans.push(Span::styled(
                SCROLL_INDICATOR_RIGHT,
                Style::default().bg(theme.tab_separator_bg),
            ));
            rendered_width += SCROLL_INDICATOR_WIDTH;
        }

        if rendered_width < max_width {
            current_spans.push(Span::styled(
                " ".repeat(max_width.saturating_sub(rendered_width)),
                Style::default().bg(theme.tab_separator_bg),
            ));
        }

        let line = Line::from(current_spans);
        let block = Block::default().style(Style::default().bg(theme.tab_separator_bg));
        let paragraph = Paragraph::new(line).block(block);
        frame.render_widget(paragraph, area);
    }

    /// Legacy render function for backward compatibility
    /// Renders all buffers as tabs (used during transition)
    #[allow(dead_code)]
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        buffers: &HashMap<BufferId, EditorState>,
        buffer_metadata: &HashMap<BufferId, BufferMetadata>,
        active_buffer: BufferId,
        theme: &crate::theme::Theme,
    ) {
        // Sort buffer IDs to ensure consistent tab order
        let mut buffer_ids: Vec<_> = buffers.keys().copied().collect();
        buffer_ids.sort_by_key(|id| id.0);

        Self::render_for_split(
            frame,
            area,
            &buffer_ids,
            buffers,
            buffer_metadata,
            active_buffer,
            theme,
            true, // Legacy behavior: always treat as active
            0,    // Default tab_scroll_offset for legacy render
        );
    }
}
