//! Split pane layout and buffer rendering

use crate::ansi::AnsiParser;
use crate::ansi_background::AnsiBackground;
use crate::cursor::SelectionMode;
use crate::editor::BufferMetadata;
use crate::event::{BufferId, EventLog, SplitDirection};
use crate::line_wrapping::{wrap_line, WrapConfig};
use crate::plugin_api::ViewTransformPayload;
use crate::split::SplitManager;
use crate::state::{EditorState, ViewMode};
use crate::ui::tabs::TabsRenderer;
use crate::virtual_text::VirtualTextPosition;
use crate::view::flatten_tokens;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

fn push_span_with_map(
    spans: &mut Vec<Span<'static>>,
    map: &mut Vec<Option<usize>>,
    text: String,
    style: Style,
    source: Option<usize>,
) {
    if text.is_empty() {
        return;
    }
    for _ in text.chars() {
        map.push(source);
    }
    spans.push(Span::styled(text, style));
}

struct ViewLine {
    offset: usize,
    text: String,
    ends_with_newline: bool,
}

struct ViewData {
    mapping: Vec<Option<usize>>,
    lines: Vec<ViewLine>,
}

struct ViewAnchor {
    start_line_idx: usize,
    start_line_skip: usize,
}

struct ComposeLayout {
    render_area: Rect,
    left_pad: u16,
    right_pad: u16,
}

struct SelectionContext {
    ranges: Vec<Range<usize>>,
    block_rects: Vec<(usize, usize, usize, usize)>,
    cursor_positions: Vec<usize>,
    primary_cursor_position: usize,
}

struct DecorationContext {
    highlight_spans: Vec<crate::highlighter::HighlightSpan>,
    semantic_spans: Vec<crate::highlighter::HighlightSpan>,
    viewport_overlays: Vec<(crate::overlay::Overlay, Range<usize>)>,
    virtual_text_lookup: HashMap<usize, Vec<crate::virtual_text::VirtualText>>,
    diagnostic_lines: HashSet<usize>,
}

struct LineRenderOutput {
    lines: Vec<Line<'static>>,
    cursor: Option<(u16, u16)>,
    last_line_end: Option<(u16, u16)>,
    content_lines_rendered: usize,
}

struct SplitLayout {
    tabs_rect: Rect,
    content_rect: Rect,
    scrollbar_rect: Rect,
}

struct ViewPreferences {
    view_mode: ViewMode,
    compose_width: Option<u16>,
    compose_column_guides: Option<Vec<u16>>,
    view_transform: Option<ViewTransformPayload>,
}

struct LineRenderInput<'a> {
    state: &'a EditorState,
    theme: &'a crate::theme::Theme,
    view_lines: &'a [ViewLine],
    view_mapping: &'a [Option<usize>],
    view_anchor: ViewAnchor,
    render_area: Rect,
    gutter_width: usize,
    selection: &'a SelectionContext,
    decorations: &'a DecorationContext,
    starting_line_num: usize,
    visible_line_count: usize,
    lsp_waiting: bool,
    is_active: bool,
    line_wrap: bool,
    estimated_lines: usize,
}

/// Renders split panes and their content
pub struct SplitRenderer;

impl SplitRenderer {
    /// Render the main content area with all splits
    ///
    /// # Arguments
    /// * `frame` - The ratatui frame to render to
    /// * `area` - The rectangular area to render in
    /// * `split_manager` - The split manager
    /// * `buffers` - All open buffers
    /// * `buffer_metadata` - Metadata for buffers (contains display names)
    /// * `event_logs` - Event logs for each buffer
    /// * `theme` - The active theme for colors
    /// * `lsp_waiting` - Whether LSP is waiting
    /// * `large_file_threshold_bytes` - Threshold for using constant scrollbar thumb size
    /// * `line_wrap` - Whether line wrapping is enabled
    /// * `estimated_line_length` - Estimated average line length for large file line estimation
    /// * `hide_cursor` - Whether to hide the hardware cursor (e.g., when menu is open)
    ///
    /// # Returns
    /// * Vec of (split_id, buffer_id, content_rect, scrollbar_rect, thumb_start, thumb_end) for mouse handling
    pub fn render_content(
        frame: &mut Frame,
        area: Rect,
        split_manager: &SplitManager,
        buffers: &mut HashMap<BufferId, EditorState>,
        buffer_metadata: &HashMap<BufferId, BufferMetadata>,
        event_logs: &mut HashMap<BufferId, EventLog>,
        theme: &crate::theme::Theme,
        ansi_background: Option<&AnsiBackground>,
        background_fade: f32,
        lsp_waiting: bool,
        large_file_threshold_bytes: u64,
        _line_wrap: bool,
        estimated_line_length: usize,
        split_view_states: Option<&HashMap<crate::event::SplitId, crate::split::SplitViewState>>,
        hide_cursor: bool,
    ) -> Vec<(crate::event::SplitId, BufferId, Rect, Rect, usize, usize)> {
        let _span = tracing::trace_span!("render_content").entered();

        // Get all visible splits with their areas
        let visible_buffers = split_manager.get_visible_buffers(area);
        let active_split_id = split_manager.active_split();

        // Collect areas for mouse handling
        let mut split_areas = Vec::new();

        // Render each split
        for (split_id, buffer_id, split_area) in visible_buffers {
            let is_active = split_id == active_split_id;

            let layout = Self::split_layout(split_area);
            let (split_buffers, tab_scroll_offset) =
                Self::split_buffers_for_tabs(split_view_states, split_id, buffer_id);

            // Render tabs for this split
            TabsRenderer::render_for_split(
                frame,
                layout.tabs_rect,
                &split_buffers,
                buffers,
                buffer_metadata,
                buffer_id, // The currently displayed buffer in this split
                theme,
                is_active,
                tab_scroll_offset,
            );

            // Get references separately to avoid double borrow
            let state_opt = buffers.get_mut(&buffer_id);
            let event_log_opt = event_logs.get_mut(&buffer_id);

            if let Some(state) = state_opt {
                let saved_state =
                    Self::temporary_split_state(state, split_view_states, split_id, is_active);
                Self::sync_viewport_to_content(state, layout.content_rect);
                let view_prefs =
                    Self::resolve_view_preferences(state, split_view_states, split_id);

                Self::render_buffer_in_split(
                    frame,
                    state,
                    event_log_opt,
                    layout.content_rect,
                    is_active,
                    theme,
                    ansi_background,
                    background_fade,
                    lsp_waiting,
                    view_prefs.view_mode,
                    view_prefs.compose_width,
                    view_prefs.compose_column_guides,
                    view_prefs.view_transform,
                    estimated_line_length,
                    buffer_id,
                    hide_cursor,
                );

                // For small files, count actual lines for accurate scrollbar
                // For large files, we'll use a constant thumb size
                // NOTE: Calculate scrollbar BEFORE restoring state to use this split's viewport
                let buffer_len = state.buffer.len();
                let (total_lines, top_line) =
                    Self::scrollbar_line_counts(state, large_file_threshold_bytes, buffer_len);

                // Render scrollbar for this split and get thumb position
                // NOTE: Render scrollbar BEFORE restoring state to use this split's viewport
                let (thumb_start, thumb_end) = Self::render_scrollbar(
                    frame,
                    state,
                    layout.scrollbar_rect,
                    is_active,
                    theme,
                    large_file_threshold_bytes,
                    total_lines,
                    top_line,
                );

                // Restore the original cursors and viewport after rendering content and scrollbar
                Self::restore_split_state(state, saved_state);

                // Store the areas for mouse handling
                split_areas.push((
                    split_id,
                    buffer_id,
                    layout.content_rect,
                    layout.scrollbar_rect,
                    thumb_start,
                    thumb_end,
                ));
            }
        }

        // Render split separators
        let separators = split_manager.get_separators(area);
        for (direction, x, y, length) in separators {
            Self::render_separator(frame, direction, x, y, length, theme);
        }

        split_areas
    }

    /// Render a split separator line
    fn render_separator(
        frame: &mut Frame,
        direction: SplitDirection,
        x: u16,
        y: u16,
        length: u16,
        theme: &crate::theme::Theme,
    ) {
        match direction {
            SplitDirection::Horizontal => {
                // Draw horizontal line
                let line_area = Rect::new(x, y, length, 1);
                let line_text = "─".repeat(length as usize);
                let paragraph =
                    Paragraph::new(line_text).style(Style::default().fg(theme.split_separator_fg));
                frame.render_widget(paragraph, line_area);
            }
            SplitDirection::Vertical => {
                // Draw vertical line
                for offset in 0..length {
                    let cell_area = Rect::new(x, y + offset, 1, 1);
                    let paragraph =
                        Paragraph::new("│").style(Style::default().fg(theme.split_separator_fg));
                    frame.render_widget(paragraph, cell_area);
                }
            }
        }
    }

    fn split_layout(split_area: Rect) -> SplitLayout {
        let tabs_height = 1u16;
        let scrollbar_width = 1u16;

        let tabs_rect = Rect::new(split_area.x, split_area.y, split_area.width, tabs_height);
        let content_rect = Rect::new(
            split_area.x,
            split_area.y + tabs_height,
            split_area.width.saturating_sub(scrollbar_width),
            split_area.height.saturating_sub(tabs_height),
        );
        let scrollbar_rect = Rect::new(
            split_area.x + split_area.width.saturating_sub(scrollbar_width),
            split_area.y + tabs_height,
            scrollbar_width,
            split_area.height.saturating_sub(tabs_height),
        );

        SplitLayout {
            tabs_rect,
            content_rect,
            scrollbar_rect,
        }
    }

    fn split_buffers_for_tabs(
        split_view_states: Option<&HashMap<crate::event::SplitId, crate::split::SplitViewState>>,
        split_id: crate::event::SplitId,
        buffer_id: BufferId,
    ) -> (Vec<BufferId>, usize) {
        if let Some(view_states) = split_view_states {
            if let Some(view_state) = view_states.get(&split_id) {
                return (
                    view_state.open_buffers.clone(),
                    view_state.tab_scroll_offset,
                );
            }
        }
        (vec![buffer_id], 0)
    }

    fn temporary_split_state(
        state: &mut EditorState,
        split_view_states: Option<&HashMap<crate::event::SplitId, crate::split::SplitViewState>>,
        split_id: crate::event::SplitId,
        is_active: bool,
    ) -> (Option<crate::cursor::Cursors>, Option<crate::viewport::Viewport>) {
        if is_active {
            return (None, None);
        }

        if let Some(view_states) = split_view_states {
            if let Some(view_state) = view_states.get(&split_id) {
                let saved_cursors =
                    Some(std::mem::replace(&mut state.cursors, view_state.cursors.clone()));
                let saved_viewport =
                    Some(std::mem::replace(&mut state.viewport, view_state.viewport.clone()));
                return (saved_cursors, saved_viewport);
            }
        }

        (None, None)
    }

    fn restore_split_state(
        state: &mut EditorState,
        saved_state: (
            Option<crate::cursor::Cursors>,
            Option<crate::viewport::Viewport>,
        ),
    ) {
        let (saved_cursors, saved_viewport) = saved_state;
        if let Some(cursors) = saved_cursors {
            state.cursors = cursors;
        }
        if let Some(viewport) = saved_viewport {
            state.viewport = viewport;
        }
    }

    fn sync_viewport_to_content(state: &mut EditorState, content_rect: Rect) {
        if state.viewport.width != content_rect.width || state.viewport.height != content_rect.height
        {
            state
                .viewport
                .resize(content_rect.width, content_rect.height);
            let primary = *state.cursors.primary();
            state.viewport.ensure_visible(&mut state.buffer, &primary);
        }
    }

    fn resolve_view_preferences(
        state: &EditorState,
        split_view_states: Option<&HashMap<crate::event::SplitId, crate::split::SplitViewState>>,
        split_id: crate::event::SplitId,
    ) -> ViewPreferences {
        if let Some(view_states) = split_view_states {
            if let Some(view_state) = view_states.get(&split_id) {
                return ViewPreferences {
                    view_mode: view_state.view_mode.clone(),
                    compose_width: view_state.compose_width,
                    compose_column_guides: view_state.compose_column_guides.clone(),
                    view_transform: view_state.view_transform.clone(),
                };
            }
        }

        ViewPreferences {
            view_mode: state.view_mode.clone(),
            compose_width: state.compose_width,
            compose_column_guides: state.compose_column_guides.clone(),
            view_transform: state.view_transform.clone(),
        }
    }

    fn scrollbar_line_counts(
        state: &EditorState,
        large_file_threshold_bytes: u64,
        buffer_len: usize,
    ) -> (usize, usize) {
        if buffer_len > large_file_threshold_bytes as usize {
            return (0, 0);
        }

        let total_lines = if buffer_len > 0 {
            state.buffer.get_line_number(buffer_len.saturating_sub(1)) + 1
        } else {
            1
        };

        let top_line = if state.viewport.top_byte < buffer_len {
            state.buffer.get_line_number(state.viewport.top_byte)
        } else {
            0
        };

        (total_lines, top_line)
    }

    /// Render a scrollbar for a split
    /// Returns (thumb_start, thumb_end) positions for mouse hit testing
    fn render_scrollbar(
        frame: &mut Frame,
        state: &EditorState,
        scrollbar_rect: Rect,
        is_active: bool,
        _theme: &crate::theme::Theme,
        large_file_threshold_bytes: u64,
        total_lines: usize,
        top_line: usize,
    ) -> (usize, usize) {
        let height = scrollbar_rect.height as usize;
        if height == 0 {
            return (0, 0);
        }

        let buffer_len = state.buffer.len();
        let viewport_top = state.viewport.top_byte;
        // Use the constant viewport height (allocated terminal rows), not visible_line_count()
        // which varies based on content. The scrollbar should represent the ratio of the
        // viewport AREA to total document size, remaining constant throughout scrolling.
        let viewport_height_lines = state.viewport.height as usize;

        // Calculate scrollbar thumb position and size
        let (thumb_start, thumb_size) = if buffer_len > large_file_threshold_bytes as usize {
            // Large file: use constant 1-character thumb for performance
            let thumb_start = if buffer_len > 0 {
                ((viewport_top as f64 / buffer_len as f64) * height as f64) as usize
            } else {
                0
            };
            (thumb_start, 1)
        } else {
            // Small file: use actual line count for accurate scrollbar
            // total_lines and top_line are passed in (already calculated with mutable access)

            // Calculate thumb size based on viewport ratio to total document
            let thumb_size_raw = if total_lines > 0 {
                ((viewport_height_lines as f64 / total_lines as f64) * height as f64).ceil()
                    as usize
            } else {
                1
            };

            // Calculate the maximum scroll position first to determine if buffer fits in viewport
            // The maximum scroll position is when the last line of the file is at
            // the bottom of the viewport, i.e., max_scroll_line = total_lines - viewport_height
            let max_scroll_line = total_lines.saturating_sub(viewport_height_lines);

            // When buffer fits entirely in viewport (no scrolling possible),
            // fill the entire scrollbar to make it obvious to the user
            let thumb_size = if max_scroll_line == 0 {
                height
            } else {
                // Cap thumb size: minimum 1, maximum 80% of scrollbar height
                let max_thumb_size = (height as f64 * 0.8).floor() as usize;
                thumb_size_raw.max(1).min(max_thumb_size).min(height)
            };

            // Calculate thumb position using proper linear mapping:
            // - At line 0: thumb_start = 0
            // - At max scroll position: thumb_start = height - thumb_size
            let thumb_start = if max_scroll_line > 0 {
                // Linear interpolation from 0 to (height - thumb_size)
                let scroll_ratio = top_line.min(max_scroll_line) as f64 / max_scroll_line as f64;
                let max_thumb_start = height.saturating_sub(thumb_size);
                (scroll_ratio * max_thumb_start as f64) as usize
            } else {
                // File fits in viewport, thumb fills entire height starting at top
                0
            };

            (thumb_start, thumb_size)
        };

        let thumb_end = thumb_start + thumb_size;

        // Choose colors based on whether split is active
        let track_color = if is_active {
            Color::DarkGray
        } else {
            Color::Black
        };
        let thumb_color = if is_active {
            Color::Gray
        } else {
            Color::DarkGray
        };

        // Render scrollbar track and thumb
        for row in 0..height {
            let cell_area = Rect::new(scrollbar_rect.x, scrollbar_rect.y + row as u16, 1, 1);

            let (char, color) = if row >= thumb_start && row < thumb_end {
                // Thumb
                ("█", thumb_color)
            } else {
                // Track
                ("│", track_color)
            };

            let paragraph = Paragraph::new(char).style(Style::default().fg(color));
            frame.render_widget(paragraph, cell_area);
        }

        // Return thumb position for mouse hit testing
        (thumb_start, thumb_end)
    }

    fn build_view_data(
        state: &mut EditorState,
        view_transform: Option<ViewTransformPayload>,
        estimated_line_length: usize,
        visible_count: usize,
    ) -> ViewData {
        if let Some(vt) = view_transform {
            let (text, mapping) = flatten_tokens(&vt.tokens);
            return ViewData {
                lines: Self::build_view_lines(&text),
                mapping,
            };
        }

        let mut text = String::new();
        let mut mapping = Vec::new();
        let mut iter = state
            .buffer
            .line_iterator(state.viewport.top_byte, estimated_line_length);
        let mut lines_seen = 0usize;
        let max_lines = visible_count.saturating_add(4);
        while lines_seen < max_lines {
            if let Some((line_start, line_content)) = iter.next() {
                let mut byte_offset = 0usize;
                for ch in line_content.chars() {
                    text.push(ch);
                    mapping.push(Some(line_start + byte_offset));
                    byte_offset += ch.len_utf8();
                }
                lines_seen += 1;
            } else {
                break;
            }
        }
        if text.is_empty() {
            mapping.push(Some(state.viewport.top_byte));
        }

        ViewData {
            lines: Self::build_view_lines(&text),
            mapping,
        }
    }

    fn build_view_lines(view_text: &str) -> Vec<ViewLine> {
        let mut view_lines: Vec<ViewLine> = Vec::new();
        let mut offset = 0usize;
        for segment in view_text.split_inclusive('\n') {
            let text = segment.to_string(); // keep newline so indices stay aligned
            let ends_with_newline = text.ends_with('\n');
            view_lines.push(ViewLine {
                offset,
                text,
                ends_with_newline,
            });
            offset += segment.chars().count();
        }
        if view_text.is_empty() {
            view_lines.push(ViewLine {
                offset: 0,
                text: String::new(),
                ends_with_newline: false,
            });
        }
        view_lines
    }

    fn calculate_view_anchor(
        view_lines: &[ViewLine],
        view_mapping: &[Option<usize>],
        top_byte: usize,
    ) -> ViewAnchor {
        let view_top = view_mapping
            .iter()
            .position(|m| m.map_or(false, |s| s >= top_byte))
            .unwrap_or(0);
        let mut view_start_line_idx = 0usize;
        let mut view_start_line_skip = 0usize;
        for (idx, line) in view_lines.iter().enumerate() {
            let len = line.text.chars().count();
            if view_top >= line.offset && view_top <= line.offset + len {
                view_start_line_idx = idx;
                view_start_line_skip = view_top.saturating_sub(line.offset);
                break;
            }
        }
        ViewAnchor {
            start_line_idx: view_start_line_idx,
            start_line_skip: view_start_line_skip,
        }
    }

    fn calculate_compose_layout(
        area: Rect,
        view_mode: &ViewMode,
        compose_width: Option<u16>,
    ) -> ComposeLayout {
        if view_mode != &ViewMode::Compose {
            return ComposeLayout {
                render_area: area,
                left_pad: 0,
                right_pad: 0,
            };
        }

        let target_width = compose_width.map(|w| w as u16).unwrap_or(area.width);
        let clamped_width = target_width.min(area.width).max(1);
        if clamped_width >= area.width {
            return ComposeLayout {
                render_area: area,
                left_pad: 0,
                right_pad: 0,
            };
        }

        let pad_total = area.width - clamped_width;
        let left_pad = pad_total / 2;
        let right_pad = pad_total - left_pad;

        ComposeLayout {
            render_area: Rect::new(area.x + left_pad, area.y, clamped_width, area.height),
            left_pad,
            right_pad,
        }
    }

    fn render_compose_margins(
        frame: &mut Frame,
        area: Rect,
        layout: &ComposeLayout,
        view_mode: &ViewMode,
        theme: &crate::theme::Theme,
    ) {
        if view_mode != &ViewMode::Compose {
            return;
        }

        let margin_style = Style::default().bg(theme.line_number_bg);
        if layout.left_pad > 0 {
            let left_rect = Rect::new(area.x, area.y, layout.left_pad, area.height);
            frame.render_widget(Block::default().style(margin_style), left_rect);
        }
        if layout.right_pad > 0 {
            let right_rect = Rect::new(
                area.x + layout.left_pad + layout.render_area.width,
                area.y,
                layout.right_pad,
                area.height,
            );
            frame.render_widget(Block::default().style(margin_style), right_rect);
        }
    }

    fn selection_context(state: &EditorState) -> SelectionContext {
        let ranges: Vec<Range<usize>> = state
            .cursors
            .iter()
            .filter_map(|(_, cursor)| cursor.selection_range())
            .collect();

        let block_rects: Vec<(usize, usize, usize, usize)> = state
            .cursors
            .iter()
            .filter_map(|(_, cursor)| {
                if cursor.selection_mode == SelectionMode::Block {
                    if let Some(anchor) = cursor.block_anchor {
                        // Convert cursor position to 2D coords
                        let cur_line = state.buffer.get_line_number(cursor.position);
                        let cur_line_start = state.buffer.line_start_offset(cur_line).unwrap_or(0);
                        let cur_col = cursor.position.saturating_sub(cur_line_start);

                        // Return normalized rectangle (min values first)
                        Some((
                            anchor.line.min(cur_line),
                            anchor.column.min(cur_col),
                            anchor.line.max(cur_line),
                            anchor.column.max(cur_col),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        let cursor_positions: Vec<usize> = if state.show_cursors {
            state
                .cursors
                .iter()
                .map(|(_, cursor)| cursor.position)
                .collect()
        } else {
            Vec::new()
        };

        SelectionContext {
            ranges,
            block_rects,
            cursor_positions,
            primary_cursor_position: state.cursors.primary().position,
        }
    }

    fn decoration_context(
        state: &mut EditorState,
        viewport_start: usize,
        viewport_end: usize,
        primary_cursor_position: usize,
    ) -> DecorationContext {
        let highlight_spans = if let Some(highlighter) = &mut state.highlighter {
            highlighter.highlight_viewport(&state.buffer, viewport_start, viewport_end)
        } else {
            Vec::new()
        };

        let semantic_spans = state.semantic_highlighter.highlight_occurrences(
            &state.buffer,
            primary_cursor_position,
            viewport_start,
            viewport_end,
        );

        let viewport_overlays = state
            .overlays
            .query_viewport(viewport_start, viewport_end, &state.marker_list)
            .into_iter()
            .map(|(overlay, range)| (overlay.clone(), range))
            .collect::<Vec<_>>();

        let diagnostic_lines: HashSet<usize> = viewport_overlays
            .iter()
            .filter_map(|(overlay, range)| {
                if let Some(id) = &overlay.id {
                    if id.starts_with("lsp-diagnostic-") {
                        return Some(state.buffer.get_line_number(range.start));
                    }
                }
                None
            })
            .collect();

        let virtual_text_lookup: HashMap<usize, Vec<crate::virtual_text::VirtualText>> = state
            .virtual_texts
            .build_lookup(&state.marker_list, viewport_start, viewport_end)
            .into_iter()
            .map(|(position, texts)| (position, texts.into_iter().cloned().collect()))
            .collect();

        DecorationContext {
            highlight_spans,
            semantic_spans,
            viewport_overlays,
            virtual_text_lookup,
            diagnostic_lines,
        }
    }

    fn calculate_viewport_end(
        state: &mut EditorState,
        viewport_start: usize,
        estimated_line_length: usize,
        visible_count: usize,
    ) -> usize {
        let mut iter_temp = state
            .buffer
            .line_iterator(viewport_start, estimated_line_length);
        let mut viewport_end = viewport_start;
        for _ in 0..visible_count {
            if let Some((line_start, line_content)) = iter_temp.next() {
                viewport_end = line_start + line_content.len();
            } else {
                break;
            }
        }
        viewport_end
    }

    fn render_view_lines(input: LineRenderInput<'_>) -> LineRenderOutput {
        let LineRenderInput {
            state,
            theme,
            view_lines,
            view_mapping,
            view_anchor,
            render_area,
            gutter_width,
            selection,
            decorations,
            starting_line_num,
            visible_line_count,
            lsp_waiting,
            is_active,
            line_wrap,
            estimated_lines,
        } = input;

        let selection_ranges = &selection.ranges;
        let block_selections = &selection.block_rects;
        let cursor_positions = &selection.cursor_positions;
        let primary_cursor_position = selection.primary_cursor_position;

        let highlight_spans = &decorations.highlight_spans;
        let semantic_spans = &decorations.semantic_spans;
        let viewport_overlays = &decorations.viewport_overlays;
        let virtual_text_lookup = &decorations.virtual_text_lookup;
        let diagnostic_lines = &decorations.diagnostic_lines;

        let mut lines = Vec::new();
        let mut lines_rendered = 0usize;
        let mut view_iter_idx = view_anchor.start_line_idx;
        let mut cursor_screen_x = 0u16;
        let mut cursor_screen_y = 0u16;
        let mut have_cursor = false;
        let mut last_line_end: Option<(u16, u16)> = None;

        let is_empty_buffer = state.buffer.is_empty();

        // Track cursor position during rendering (eliminates duplicate line iteration)
        let mut last_visible_x: u16 = 0;
        let mut view_start_line_skip = view_anchor.start_line_skip;

        loop {
            let (line_view_offset, line_content, line_has_newline) =
                if let Some(ViewLine {
                    offset,
                    text,
                    ends_with_newline,
                }) = view_lines.get(view_iter_idx)
                {
                    let mut content = text.clone();
                    let mut base = *offset;
                    if view_iter_idx == view_anchor.start_line_idx && view_start_line_skip > 0 {
                        let skip = view_start_line_skip;
                        content = text.chars().skip(skip).collect();
                        base += skip;
                        view_start_line_skip = 0;
                    }
                    view_iter_idx += 1;
                    (base, content, *ends_with_newline)
                } else if is_empty_buffer && lines_rendered == 0 {
                    (0, String::new(), false)
                } else {
                    break;
                };

            if lines_rendered >= visible_line_count {
                break;
            }

            let current_line_num = starting_line_num + lines_rendered;
            lines_rendered += 1;

            // Apply horizontal scrolling - skip characters before left_column
            let left_col = state.viewport.left_column;

            // Build line with selection highlighting
            let mut line_spans = Vec::new();
            let mut line_view_map: Vec<Option<usize>> = Vec::new();
            let mut last_seg_y: Option<u16> = None;
            let mut _last_seg_width: usize = 0;

            // Render left margin (indicators + line numbers + separator)
            if state.margins.left_config.enabled {
                if diagnostic_lines.contains(&current_line_num) {
                    // Show diagnostic indicator
                    push_span_with_map(
                        &mut line_spans,
                        &mut line_view_map,
                        "●".to_string(),
                        Style::default().fg(ratatui::style::Color::Red),
                        None,
                    );
                } else {
                    // Show space (reserved for future indicators like breakpoints)
                    push_span_with_map(
                        &mut line_spans,
                        &mut line_view_map,
                        " ".to_string(),
                        Style::default(),
                        None,
                    );
                }

                // Next N columns: render line number (right-aligned)
                let margin_content = state.margins.render_line(
                    current_line_num,
                    crate::margin::MarginPosition::Left,
                    estimated_lines,
                );
                let (rendered_text, style_opt) =
                    margin_content.render(state.margins.left_config.width);

                // Use custom style if provided, otherwise use default theme color
                let margin_style =
                    style_opt.unwrap_or_else(|| Style::default().fg(theme.line_number_fg));

                push_span_with_map(
                    &mut line_spans,
                    &mut line_view_map,
                    rendered_text,
                    margin_style,
                    None,
                );

                // Render separator
                if state.margins.left_config.show_separator {
                    let separator_style = Style::default().fg(theme.line_number_fg);
                    push_span_with_map(
                        &mut line_spans,
                        &mut line_view_map,
                        state.margins.left_config.separator.clone(),
                        separator_style,
                        None,
                    );
                }
            }

            // Check if this line has any selected text
            let mut char_index = 0;
            let mut col_offset = 0usize;

            // Performance optimization: For very long lines, only process visible characters
            // Calculate the maximum characters we might need to render based on screen width
            // For wrapped lines, we need enough characters to fill the visible viewport
            // For non-wrapped lines, we only need one screen width worth
            let visible_lines_remaining = visible_line_count.saturating_sub(lines_rendered);
            let max_visible_chars = if line_wrap {
                // With wrapping: might need chars for multiple wrapped lines
                // Be generous to avoid cutting off wrapped content
                (render_area.width as usize)
                    .saturating_mul(visible_lines_remaining.max(1))
                    .saturating_add(200)
            } else {
                // Without wrapping: only need one line worth of characters
                (render_area.width as usize).saturating_add(100)
            };
            let max_chars_to_process = left_col.saturating_add(max_visible_chars);

            // ANSI parser for this line to handle escape sequences
            let mut ansi_parser = AnsiParser::new();
            // Track visible characters separately from byte position for ANSI handling
            let mut visible_char_count = 0usize;

            let mut chars_iterator = line_content.chars().peekable();
            while let Some(ch) = chars_iterator.next() {
                let view_idx = line_view_offset + col_offset;
                let byte_pos = view_mapping.get(view_idx).copied().flatten();

                // Process character through ANSI parser first
                // If it returns None, the character is part of an escape sequence and should be skipped
                let ansi_style = match ansi_parser.parse_char(ch) {
                    Some(style) => style,
                    None => {
                        // This character is part of an ANSI escape sequence, skip it
                        char_index += ch.len_utf8();
                        continue;
                    }
                };

                // Performance: skip expensive style calculations for characters beyond visible range
                // Use visible_char_count (not char_index) since ANSI codes don't take up visible space
                if visible_char_count > max_chars_to_process {
                    // Fast path: just count remaining characters without processing
                    // This is critical for performance with very long lines (e.g., 100KB single line)
                    char_index += ch.len_utf8();
                    for remaining_ch in chars_iterator.by_ref() {
                        char_index += remaining_ch.len_utf8();
                    }
                    break;
                }

                // Skip characters before left_column
                if col_offset >= left_col as usize {
                    // Check if this character is at a cursor position
                    let is_cursor = byte_pos
                        .map(|bp| bp < state.buffer.len() && cursor_positions.contains(&bp))
                        .unwrap_or(false);

                    // Check if this character is in any selection range (but not at cursor position)
                    // Also check for block/rectangular selections
                    let is_in_block_selection = block_selections.iter().any(
                        |(start_line, start_col, end_line, end_col)| {
                            current_line_num >= *start_line
                                && current_line_num <= *end_line
                                && char_index >= *start_col
                                && char_index <= *end_col
                        },
                    );

                    let is_selected = !is_cursor
                        && byte_pos.map_or(false, |bp| {
                            selection_ranges
                                .iter()
                                .any(|range| range.contains(&bp))
                        })
                        || (!is_cursor && is_in_block_selection);

                    let highlight_color = byte_pos.and_then(|bp| {
                        highlight_spans
                            .iter()
                            .find(|span| span.range.contains(&bp))
                            .map(|span| span.color)
                    });

                    let overlays: Vec<&crate::overlay::Overlay> = if let Some(bp) = byte_pos {
                        viewport_overlays
                            .iter()
                            .filter(|(_, range)| range.contains(&bp))
                            .map(|(overlay, _)| overlay)
                            .collect()
                    } else {
                        Vec::new()
                    };

                    // Build style by layering: base -> ansi -> syntax -> semantic -> overlays -> selection
                    // Start with ANSI style as base (if present), otherwise use theme default
                    let mut style = if ansi_style.fg.is_some()
                        || ansi_style.bg.is_some()
                        || !ansi_style.add_modifier.is_empty()
                    {
                        // Apply ANSI styling from escape codes
                        let mut s = Style::default();
                        if let Some(fg) = ansi_style.fg {
                            s = s.fg(fg);
                        } else {
                            s = s.fg(theme.editor_fg);
                        }
                        if let Some(bg) = ansi_style.bg {
                            s = s.bg(bg);
                        }
                        s = s.add_modifier(ansi_style.add_modifier);
                        s
                    } else if let Some(color) = highlight_color {
                        // Apply syntax highlighting
                        Style::default().fg(color)
                    } else {
                        // Default color from theme
                        Style::default().fg(theme.editor_fg)
                    };

                    // If we have ANSI style but also syntax highlighting, syntax takes precedence for color
                    // (unless ANSI has explicit color which we already applied above)
                    if highlight_color.is_some()
                        && ansi_style.fg.is_none()
                        && (ansi_style.bg.is_some() || !ansi_style.add_modifier.is_empty())
                    {
                        // ANSI had bg or modifiers but not fg, so apply syntax fg
                        style = style.fg(highlight_color.unwrap());
                    }

                    if let Some(bp) = byte_pos {
                        if let Some(semantic_span) =
                            semantic_spans.iter().find(|span| span.range.contains(&bp))
                        {
                            style = style.bg(semantic_span.color);
                        }
                    }

                    use crate::overlay::OverlayFace;
                    for overlay in &overlays {
                        match &overlay.face {
                            OverlayFace::Underline {
                                color,
                                style: _underline_style,
                            } => {
                                style = style.add_modifier(Modifier::UNDERLINED).fg(*color);
                            }
                            OverlayFace::Background { color } => {
                                style = style.bg(*color);
                            }
                            OverlayFace::Foreground { color } => {
                                style = style.fg(*color);
                            }
                            OverlayFace::Style {
                                style: overlay_style,
                            } => {
                                style = style.patch(*overlay_style);
                            }
                        }
                    }

                    if is_selected {
                        style = Style::default().fg(theme.editor_fg).bg(theme.selection_bg);
                    }

                    // Cursor styling - make secondary cursors visible with reversed colors
                    // Don't apply REVERSED to primary cursor to preserve terminal cursor visibility
                    // For inactive splits, ALL cursors use a less pronounced color (no hardware cursor)
                    let is_secondary_cursor =
                        is_cursor && byte_pos != Some(primary_cursor_position);
                    if is_active {
                        if is_secondary_cursor {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                    } else if is_cursor {
                        style = style.fg(theme.editor_fg).bg(theme.inactive_cursor);
                    }

                    let display_char = if is_cursor && lsp_waiting && is_active {
                        "⋯"
                    } else if is_cursor && is_active && ch == '\n' {
                        ""
                    } else if ch == '\n' {
                        ""
                    } else {
                        &ch.to_string()
                    };

                    if let Some(bp) = byte_pos {
                        if let Some(vtexts) = virtual_text_lookup.get(&bp) {
                            for vtext in vtexts
                                .iter()
                                .filter(|v| v.position == VirtualTextPosition::BeforeChar)
                            {
                                let text_with_space = format!("{} ", vtext.text);
                                push_span_with_map(
                                    &mut line_spans,
                                    &mut line_view_map,
                                    text_with_space,
                                    vtext.style,
                                    None,
                                );
                            }
                        }
                    }

                    if !display_char.is_empty() {
                        push_span_with_map(
                            &mut line_spans,
                            &mut line_view_map,
                            display_char.to_string(),
                            style,
                            byte_pos,
                        );
                    }

                    if let Some(bp) = byte_pos {
                        if let Some(vtexts) = virtual_text_lookup.get(&bp) {
                            for vtext in vtexts
                                .iter()
                                .filter(|v| v.position == VirtualTextPosition::AfterChar)
                            {
                                let text_with_space = format!(" {}", vtext.text);
                                push_span_with_map(
                                    &mut line_spans,
                                    &mut line_view_map,
                                    text_with_space,
                                    vtext.style,
                                    None,
                                );
                            }
                        }
                    }

                    if is_cursor && ch == '\n' {
                        let should_add_indicator = if is_active {
                            is_secondary_cursor
                        } else {
                            true
                        };
                        if should_add_indicator {
                            let cursor_style = if is_active {
                                Style::default()
                                    .fg(theme.editor_fg)
                                    .bg(theme.editor_bg)
                                    .add_modifier(Modifier::REVERSED)
                            } else {
                                Style::default()
                                    .fg(theme.editor_fg)
                                    .bg(theme.inactive_cursor)
                            };
                            push_span_with_map(
                                &mut line_spans,
                                &mut line_view_map,
                                " ".to_string(),
                                cursor_style,
                                byte_pos,
                            );
                        }
                    }
                }

                char_index += ch.len_utf8();
                col_offset += 1;
                visible_char_count += 1;
            }

            if !line_has_newline {
                let line_len_chars = line_content.chars().count();
                let line_end_pos =
                    line_view_offset + line_len_chars.saturating_sub(1);
                let cursor_at_end = cursor_positions.iter().any(|&pos| {
                    pos == line_end_pos || pos == line_view_offset + line_len_chars
                });

                if cursor_at_end {
                    let is_primary_at_end = line_end_pos == primary_cursor_position;
                    let should_add_indicator = if is_active {
                        !is_primary_at_end
                    } else {
                        true
                    };
                    if should_add_indicator {
                        let cursor_style = if is_active {
                            Style::default()
                                .fg(theme.editor_fg)
                                .bg(theme.editor_bg)
                                .add_modifier(Modifier::REVERSED)
                        } else {
                            Style::default()
                                .fg(theme.editor_fg)
                                .bg(theme.inactive_cursor)
                        };
                        push_span_with_map(
                            &mut line_spans,
                            &mut line_view_map,
                            " ".to_string(),
                            cursor_style,
                            None,
                        );
                    }
                }
            }

            if !line_spans.is_empty() {
                let config = if line_wrap {
                    WrapConfig::new(render_area.width as usize, gutter_width, true)
                } else {
                    WrapConfig::no_wrap(gutter_width)
                };

                // Separate gutter spans from content spans
                // Count characters in gutter to find where content starts
                let mut gutter_char_count = 0;
                let mut gutter_span_count = 0;
                for span in &line_spans {
                    let span_len = span.content.chars().count();
                    if gutter_char_count + span_len <= gutter_width {
                        gutter_char_count += span_len;
                        gutter_span_count += 1;
                    } else {
                        break;
                    }
                }

                // Extract only the content spans (skip gutter spans)
                let content_spans = &line_spans[gutter_span_count..];
                let line_text: String =
                    content_spans.iter().map(|s| s.content.as_ref()).collect();
                let content_view_map = if line_view_map.len() > gutter_char_count {
                    line_view_map[gutter_char_count..].to_vec()
                } else {
                    Vec::new()
                };

                let segments = wrap_line(&line_text, &config);

                // Render each wrapped segment
                for (seg_idx, segment) in segments.iter().enumerate() {
                    let mut segment_spans = vec![];

                    // Add gutter for each segment
                    if seg_idx == 0 {
                        // First segment gets the actual gutter (line numbers, etc.)
                        segment_spans.extend_from_slice(&line_spans[..gutter_span_count]);
                    } else {
                        // Continuation lines get spaces in the gutter area
                        push_span_with_map(
                            &mut segment_spans,
                            &mut line_view_map,
                            " ".repeat(gutter_width),
                            Style::default(),
                            None,
                        );
                    }

                    let segment_text = segment.text.clone();
                    _last_seg_width = segment_text.chars().count();

                    let styled_spans = Self::apply_styles_to_segment(
                        &segment_text,
                        content_spans,
                        segment.start_char_offset,
                        if !line_wrap { left_col } else { 0 },
                    );
                    segment_spans.extend(styled_spans);

                    let current_y = lines.len() as u16;
                    last_seg_y = Some(current_y);
                    for (i, ch) in segment_text.chars().enumerate() {
                        if ch == '\n' {
                            continue;
                        }
                        if let Some(Some(src)) =
                            content_view_map.get(segment.start_char_offset + i)
                        {
                            let screen_x = i as u16;
                            last_visible_x = screen_x;
                            if *src == primary_cursor_position {
                                cursor_screen_x = screen_x;
                                cursor_screen_y = current_y;
                                have_cursor = true;
                            }
                        }
                    }

                    lines.push(Line::from(segment_spans));
                    lines_rendered += 1;

                    if lines_rendered >= visible_line_count {
                        break;
                    }
                }

                lines_rendered = lines_rendered.saturating_sub(1);
            } else {
                lines.push(Line::from(line_spans));
            }

            if lines_rendered >= visible_line_count {
                break;
            }

            if let Some(y) = last_seg_y {
                let end_x = last_visible_x.saturating_add(1);
                let view_end_idx = line_view_offset + line_content.chars().count();

                last_line_end = Some((end_x, y));

                if line_has_newline && line_content.chars().count() > 0 {
                    let newline_idx = view_end_idx.saturating_sub(1);
                    if let Some(Some(src_newline)) = view_mapping.get(newline_idx) {
                        if *src_newline == primary_cursor_position {
                            cursor_screen_x = end_x;
                            cursor_screen_y = y;
                            have_cursor = true;
                        }
                    }
                }
            }
        }

        while lines.len() < render_area.height as usize {
            lines.push(Line::raw(""));
        }

        LineRenderOutput {
            lines,
            cursor: have_cursor.then_some((cursor_screen_x, cursor_screen_y)),
            last_line_end,
            content_lines_rendered: lines_rendered,
        }
    }

    fn resolve_cursor_fallback(
        current_cursor: Option<(u16, u16)>,
        primary_cursor_position: usize,
        buffer_len: usize,
        buffer_ends_with_newline: bool,
        last_line_end: Option<(u16, u16)>,
        lines_rendered: usize,
    ) -> Option<(u16, u16)> {
        if current_cursor.is_some() || primary_cursor_position != buffer_len {
            return current_cursor;
        }

        if buffer_ends_with_newline {
            return Some((0, lines_rendered.saturating_sub(1) as u16));
        }

        last_line_end
    }


    /// Render a single buffer in a split pane
    fn render_buffer_in_split(
        frame: &mut Frame,
        state: &mut EditorState,
        event_log: Option<&mut EventLog>,
        area: Rect,
        is_active: bool,
        theme: &crate::theme::Theme,
        ansi_background: Option<&AnsiBackground>,
        background_fade: f32,
        lsp_waiting: bool,
        view_mode: ViewMode,
        compose_width: Option<u16>,
        _compose_column_guides: Option<Vec<u16>>,
        view_transform: Option<ViewTransformPayload>,
        estimated_line_length: usize,
        _buffer_id: BufferId,
        hide_cursor: bool,
    ) {
        let _span = tracing::trace_span!("render_buffer_in_split").entered();

        let line_wrap = state.viewport.line_wrap_enabled;

        let overlay_count = state.overlays.all().len();
        if overlay_count > 0 {
            tracing::trace!("render_content: {} overlays present", overlay_count);
        }

        let visible_count = state.viewport.visible_line_count();

        let view_data = Self::build_view_data(state, view_transform, estimated_line_length, visible_count);
        let view_anchor =
            Self::calculate_view_anchor(&view_data.lines, &view_data.mapping, state.viewport.top_byte);

        let buffer_len = state.buffer.len();
        let estimated_lines = (buffer_len / 80).max(1);
        state.margins.update_width_for_buffer(estimated_lines);
        let gutter_width = state.margins.left_total_width();

        let compose_layout = Self::calculate_compose_layout(area, &view_mode, compose_width);
        let render_area = compose_layout.render_area;
        Self::render_compose_margins(frame, area, &compose_layout, &view_mode, theme);

        let selection = Self::selection_context(state);

        tracing::trace!(
            "Rendering buffer with {} cursors at positions: {:?}, primary at {}, is_active: {}, buffer_len: {}",
            selection.cursor_positions.len(),
            selection.cursor_positions,
            selection.primary_cursor_position,
            is_active,
            state.buffer.len()
        );

        if !selection
            .cursor_positions
            .contains(&selection.primary_cursor_position)
        {
            tracing::warn!(
                "Primary cursor position {} not found in cursor_positions list: {:?}",
                selection.primary_cursor_position,
                selection.cursor_positions
            );
        }

        let starting_line_num =
            state
                .buffer
                .populate_line_cache(state.viewport.top_byte, visible_count);

        let viewport_start = state.viewport.top_byte;
        let viewport_end = Self::calculate_viewport_end(
            state,
            viewport_start,
            estimated_line_length,
            visible_count,
        );

        let decorations = Self::decoration_context(
            state,
            viewport_start,
            viewport_end,
            selection.primary_cursor_position,
        );

        let render_output = Self::render_view_lines(LineRenderInput {
            state,
            theme,
            view_lines: &view_data.lines,
            view_mapping: &view_data.mapping,
            view_anchor,
            render_area,
            gutter_width,
            selection: &selection,
            decorations: &decorations,
            starting_line_num,
            visible_line_count: visible_count,
            lsp_waiting,
            is_active,
            line_wrap,
            estimated_lines,
        });

        let mut lines = render_output.lines;
        let background_x_offset = state.viewport.left_column as usize;

        if let Some(bg) = ansi_background {
            Self::apply_background_to_lines(
                &mut lines,
                render_area.width,
                bg,
                theme.editor_bg,
                theme.editor_fg,
                background_fade,
                background_x_offset,
                starting_line_num,
            );
        }

        frame.render_widget(Clear, render_area);
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::NONE)),
            render_area,
        );

        let buffer_ends_with_newline = if state.buffer.len() > 0 {
            let last_char = state.get_text_range(state.buffer.len() - 1, state.buffer.len());
            last_char == "\\n"
        } else {
            false
        };

        let cursor = Self::resolve_cursor_fallback(
            render_output.cursor,
            selection.primary_cursor_position,
            state.buffer.len(),
            buffer_ends_with_newline,
            render_output.last_line_end,
            render_output.content_lines_rendered,
        );

        if is_active && state.show_cursors && !hide_cursor {
            if let Some((cursor_screen_x, cursor_screen_y)) = cursor {
                let screen_x = render_area
                    .x
                    .saturating_add(cursor_screen_x)
                    .saturating_add(gutter_width as u16);
                let screen_y = render_area.y.saturating_add(cursor_screen_y);

                frame.set_cursor_position((screen_x, screen_y));

                if let Some(event_log) = event_log {
                    let cursor_pos = state.cursors.primary().position;
                    let buffer_len = state.buffer.len();
                    event_log.log_render_state(cursor_pos, screen_x, screen_y, buffer_len);
                }
            }
        }
    }

    /// Apply styles from original line_spans to a wrapped segment
    ///
    /// Maps each character in the segment text back to its original span to preserve
    /// syntax highlighting, selections, and other styling across wrapped lines.
    ///
    /// # Arguments
    /// * `segment_text` - The text content of this wrapped segment
    /// * `line_spans` - The original styled spans for the entire line
    /// * `segment_start_offset` - Character offset where this segment starts in the original line
    /// * `scroll_offset` - Additional offset for horizontal scrolling (non-wrap mode)
    fn apply_styles_to_segment(
        segment_text: &str,
        line_spans: &[Span<'static>],
        segment_start_offset: usize,
        _scroll_offset: usize,
    ) -> Vec<Span<'static>> {
        if line_spans.is_empty() {
            return vec![Span::raw(segment_text.to_string())];
        }

        let mut result_spans = Vec::new();
        let segment_chars: Vec<char> = segment_text.chars().collect();

        if segment_chars.is_empty() {
            return vec![Span::raw(String::new())];
        }

        // Build a map of character position -> style
        let mut char_styles: Vec<(char, Style)> = Vec::new();

        for span in line_spans {
            let span_text = span.content.as_ref();
            let style = span.style;

            for ch in span_text.chars() {
                char_styles.push((ch, style));
            }
        }

        // Extract the styles for this segment
        let mut current_text = String::new();
        let mut current_style = None;

        for (i, &ch) in segment_chars.iter().enumerate() {
            // segment_start_offset is relative to the line_text (which already accounts for scrolling),
            // so don't add scroll_offset again - it would double-count the horizontal scrolling
            let original_pos = segment_start_offset + i;

            let style_for_char = if original_pos < char_styles.len() {
                char_styles[original_pos].1
            } else {
                Style::default()
            };

            // If style changed, flush current span and start new one
            if let Some(prev_style) = current_style {
                if prev_style != style_for_char {
                    result_spans.push(Span::styled(current_text.clone(), prev_style));
                    current_text.clear();
                    current_style = Some(style_for_char);
                }
            } else {
                current_style = Some(style_for_char);
            }

            current_text.push(ch);
        }

        // Flush remaining text
        if !current_text.is_empty() {
            if let Some(style) = current_style {
                result_spans.push(Span::styled(current_text, style));
            }
        }

        if result_spans.is_empty() {
            vec![Span::raw(String::new())]
        } else {
            result_spans
        }
    }

    fn apply_background_to_lines(
        lines: &mut Vec<Line<'static>>,
        area_width: u16,
        background: &AnsiBackground,
        theme_bg: Color,
        default_fg: Color,
        fade: f32,
        x_offset: usize,
        y_offset: usize,
    ) {
        if area_width == 0 {
            return;
        }

        let width = area_width as usize;

        for (y, line) in lines.iter_mut().enumerate() {
            // Flatten existing spans into per-character styles
            let mut existing: Vec<(char, Style)> = Vec::new();
            let spans = std::mem::take(&mut line.spans);
            for span in spans {
                let style = span.style;
                for ch in span.content.chars() {
                    existing.push((ch, style));
                }
            }

            let mut chars_with_style = Vec::with_capacity(width);
            for x in 0..width {
                let sample_x = x_offset + x;
                let sample_y = y_offset + y;

                let (ch, mut style) = if x < existing.len() {
                    existing[x]
                } else {
                    (' ', Style::default().fg(default_fg))
                };

                if let Some(bg_color) = background.faded_color(sample_x, sample_y, theme_bg, fade) {
                    if style.bg.is_none() || matches!(style.bg, Some(Color::Reset)) {
                        style = style.bg(bg_color);
                    }
                }

                chars_with_style.push((ch, style));
            }

            line.spans = Self::compress_chars(chars_with_style);
        }
    }

    fn compress_chars(chars: Vec<(char, Style)>) -> Vec<Span<'static>> {
        if chars.is_empty() {
            return vec![];
        }

        let mut spans = Vec::new();
        let mut current_style = chars[0].1;
        let mut current_text = String::new();
        current_text.push(chars[0].0);

        for (ch, style) in chars.into_iter().skip(1) {
            if style == current_style {
                current_text.push(ch);
            } else {
                spans.push(Span::styled(current_text.clone(), current_style));
                current_text.clear();
                current_text.push(ch);
                current_style = style;
            }
        }

        spans.push(Span::styled(current_text, current_style));
        spans
    }
}
