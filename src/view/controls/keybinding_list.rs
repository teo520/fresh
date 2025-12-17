//! Keybinding list control for displaying and editing keybindings

use super::FocusState;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use serde_json::Value;

/// State for a keybinding list control
#[derive(Debug, Clone)]
pub struct KeybindingListState {
    /// List of keybindings as JSON values
    pub bindings: Vec<Value>,
    /// Currently focused binding index (None = add-new row)
    pub focused_index: Option<usize>,
    /// Label for this control
    pub label: String,
    /// Focus state
    pub focus: FocusState,
}

impl KeybindingListState {
    /// Create a new keybinding list state
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            bindings: Vec::new(),
            focused_index: None,
            label: label.into(),
            focus: FocusState::Normal,
        }
    }

    /// Initialize from a JSON array value
    pub fn with_bindings(mut self, value: &Value) -> Self {
        if let Some(arr) = value.as_array() {
            self.bindings = arr.clone();
        }
        self
    }

    /// Convert to JSON value
    pub fn to_value(&self) -> Value {
        Value::Array(self.bindings.clone())
    }

    /// Get the focused binding
    pub fn focused_binding(&self) -> Option<&Value> {
        self.focused_index.and_then(|idx| self.bindings.get(idx))
    }

    /// Focus next entry
    pub fn focus_next(&mut self) {
        match self.focused_index {
            None => {
                // From add-new to first entry (if any)
                if !self.bindings.is_empty() {
                    self.focused_index = Some(0);
                }
            }
            Some(idx) if idx + 1 < self.bindings.len() => {
                self.focused_index = Some(idx + 1);
            }
            Some(_) => {
                // Last entry, go to add-new
                self.focused_index = None;
            }
        }
    }

    /// Focus previous entry
    pub fn focus_prev(&mut self) {
        match self.focused_index {
            None => {
                // From add-new to last entry (if any)
                if !self.bindings.is_empty() {
                    self.focused_index = Some(self.bindings.len() - 1);
                }
            }
            Some(0) => {
                // First entry stays at first
            }
            Some(idx) => {
                self.focused_index = Some(idx - 1);
            }
        }
    }

    /// Remove the focused binding
    pub fn remove_focused(&mut self) {
        if let Some(idx) = self.focused_index {
            if idx < self.bindings.len() {
                self.bindings.remove(idx);
                // Adjust focus
                if self.bindings.is_empty() {
                    self.focused_index = None;
                } else if idx >= self.bindings.len() {
                    self.focused_index = Some(self.bindings.len() - 1);
                }
            }
        }
    }

    /// Add a new binding
    pub fn add_binding(&mut self, binding: Value) {
        self.bindings.push(binding);
    }

    /// Update the binding at index
    pub fn update_binding(&mut self, index: usize, binding: Value) {
        if index < self.bindings.len() {
            self.bindings[index] = binding;
        }
    }
}

/// Colors for keybinding list rendering
#[derive(Debug, Clone, Copy)]
pub struct KeybindingListColors {
    pub label_fg: Color,
    pub key_fg: Color,
    pub action_fg: Color,
    pub focused_bg: Color,
    pub delete_fg: Color,
    pub add_fg: Color,
}

impl Default for KeybindingListColors {
    fn default() -> Self {
        Self {
            label_fg: Color::White,
            key_fg: Color::Yellow,
            action_fg: Color::Cyan,
            focused_bg: Color::DarkGray,
            delete_fg: Color::Red,
            add_fg: Color::Green,
        }
    }
}

/// Layout information for hit testing
#[derive(Debug, Clone)]
pub struct KeybindingListLayout {
    pub entry_rects: Vec<Rect>,
    pub delete_rects: Vec<Rect>,
    pub add_rect: Option<Rect>,
}

/// Format a keybinding's key combination for display
pub fn format_key_combo(binding: &Value) -> String {
    // Check for keys array (chord binding) first
    if let Some(keys) = binding.get("keys").and_then(|k| k.as_array()) {
        let parts: Vec<String> = keys
            .iter()
            .map(|k| {
                let mut key_str = String::new();
                if let Some(mods) = k.get("modifiers").and_then(|m| m.as_array()) {
                    for m in mods {
                        if let Some(s) = m.as_str() {
                            key_str.push_str(&capitalize_mod(s));
                            key_str.push('+');
                        }
                    }
                }
                if let Some(key) = k.get("key").and_then(|k| k.as_str()) {
                    key_str.push_str(&capitalize_key(key));
                }
                key_str
            })
            .collect();
        return parts.join(" ");
    }

    // Single key binding
    let mut result = String::new();
    if let Some(mods) = binding.get("modifiers").and_then(|m| m.as_array()) {
        for m in mods {
            if let Some(s) = m.as_str() {
                result.push_str(&capitalize_mod(s));
                result.push('+');
            }
        }
    }
    if let Some(key) = binding.get("key").and_then(|k| k.as_str()) {
        result.push_str(&capitalize_key(key));
    }
    result
}

fn capitalize_mod(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "ctrl" | "control" => "Ctrl".to_string(),
        "alt" => "Alt".to_string(),
        "shift" => "Shift".to_string(),
        "super" | "meta" | "cmd" => "Super".to_string(),
        _ => s.to_string(),
    }
}

fn capitalize_key(s: &str) -> String {
    if s.len() == 1 {
        s.to_uppercase()
    } else {
        let mut chars = s.chars();
        match chars.next() {
            None => String::new(),
            Some(c) => c.to_uppercase().chain(chars).collect(),
        }
    }
}

/// Render a keybinding list control
pub fn render_keybinding_list(
    frame: &mut Frame,
    area: Rect,
    state: &KeybindingListState,
    colors: &KeybindingListColors,
) -> KeybindingListLayout {
    let mut layout = KeybindingListLayout {
        entry_rects: Vec::new(),
        delete_rects: Vec::new(),
        add_rect: None,
    };

    let is_focused = state.focus == FocusState::Focused;

    // Render label
    let label_line = Line::from(vec![Span::styled(
        format!("{}:", state.label),
        Style::default().fg(colors.label_fg),
    )]);
    frame.render_widget(Paragraph::new(label_line), area);

    // Render entries
    for (idx, binding) in state.bindings.iter().enumerate() {
        let y = area.y + 1 + idx as u16;
        if y >= area.y + area.height {
            break;
        }

        let entry_area = Rect::new(area.x + 2, y, area.width.saturating_sub(2), 1);
        layout.entry_rects.push(entry_area);

        let is_entry_focused = is_focused && state.focused_index == Some(idx);
        let bg = if is_entry_focused {
            colors.focused_bg
        } else {
            Color::Reset
        };

        let key_combo = format_key_combo(binding);
        let action = binding
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("(no action)");

        let indicator = if is_entry_focused { "> " } else { "  " };
        let line = Line::from(vec![
            Span::styled(indicator, Style::default().fg(colors.label_fg).bg(bg)),
            Span::styled(
                format!("{:<20}", key_combo),
                Style::default().fg(colors.key_fg).bg(bg),
            ),
            Span::styled(" → ", Style::default().fg(colors.label_fg).bg(bg)),
            Span::styled(action, Style::default().fg(colors.action_fg).bg(bg)),
            Span::styled(" [x]", Style::default().fg(colors.delete_fg).bg(bg)),
        ]);
        frame.render_widget(Paragraph::new(line), entry_area);

        // Track delete button area
        let delete_x = entry_area.x + entry_area.width.saturating_sub(4);
        layout.delete_rects.push(Rect::new(delete_x, y, 3, 1));
    }

    // Render add-new row
    let add_y = area.y + 1 + state.bindings.len() as u16;
    if add_y < area.y + area.height {
        let add_area = Rect::new(area.x + 2, add_y, area.width.saturating_sub(2), 1);
        layout.add_rect = Some(add_area);

        let is_add_focused = is_focused && state.focused_index.is_none();
        let bg = if is_add_focused {
            colors.focused_bg
        } else {
            Color::Reset
        };

        let indicator = if is_add_focused { "> " } else { "  " };
        let line = Line::from(vec![
            Span::styled(indicator, Style::default().fg(colors.label_fg).bg(bg)),
            Span::styled("[+] Add new", Style::default().fg(colors.add_fg).bg(bg)),
        ]);
        frame.render_widget(Paragraph::new(line), add_area);
    }

    layout
}
