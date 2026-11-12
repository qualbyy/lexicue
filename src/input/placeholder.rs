//! `{{arg}}` placeholder templates.
//!
//! Syntax: `{{name}}` where `name` is `[A-Za-z0-9_-]+`. Placeholders are the only
//! editable regions; the rest of the command is immutable. Tab advances slots,
//! Enter confirms. Unfilled slots render as the literal `{{name}}`.

use std::ops::Range;

/// One editable region, byte-indexed into [`Template::raw`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub span: Range<usize>,
    /// Current value; empty means unfilled.
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub raw: String,
    pub slots: Vec<Slot>,
    /// Index into `slots` that receives typed characters.
    pub active: usize,
}

impl Template {
    /// Missing slots are allowed (plain command = zero-slot template).
    pub fn parse(raw: &str) -> Result<Self, TemplateError> {
        let mut slots = Vec::new();
        let bytes = raw.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if raw[i..].starts_with("{{") {
                let start = i;
                let after_open = i + 2;
                let Some(rel_close) = raw[after_open..].find("}}") else {
                    return Err(TemplateError::Unclosed(start));
                };
                let close = after_open + rel_close;
                if let Some(rel_nested) = raw[after_open..close].find("{{") {
                    return Err(TemplateError::Nested(after_open + rel_nested));
                }
                let name = &raw[after_open..close];
                if name.is_empty() {
                    return Err(TemplateError::EmptyName(start));
                }
                if !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(TemplateError::InvalidName(start));
                }
                slots.push(Slot {
                    name: name.to_string(),
                    span: start..(close + 2),
                    value: String::new(),
                });
                i = close + 2;
            } else {
                let ch_len = raw[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                i += ch_len;
            }
        }

        Ok(Self {
            raw: raw.to_string(),
            slots,
            active: 0,
        })
    }

    pub fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    pub fn active_slot(&self) -> Option<&Slot> {
        self.slots.get(self.active)
    }

    /// Sets the whole active slot value at once.
    #[allow(dead_code)] // Shortcut: сейчас только тесты; пригодится для вставки из буфера.
    pub fn fill_active(&mut self, value: &str) {
        if let Some(slot) = self.slots.get_mut(self.active) {
            slot.value.clear();
            slot.value.push_str(value);
        }
    }

    pub fn push_active_char(&mut self, ch: char) {
        if let Some(slot) = self.slots.get_mut(self.active) {
            slot.value.push(ch);
        }
    }

    pub fn pop_active_char(&mut self) {
        if let Some(slot) = self.slots.get_mut(self.active) {
            slot.value.pop();
        }
    }

    /// Moves focus to the next slot; returns `false` when already on the last one
    /// (the caller then treats Enter as confirm).
    pub fn focus_next(&mut self) -> bool {
        if self.active + 1 < self.slots.len() {
            self.active += 1;
            true
        } else {
            false
        }
    }

    /// Substitutes filled slots; unfilled ones stay literal for preview clarity.
    pub fn render(&self) -> String {
        if self.slots.is_empty() {
            return self.raw.clone();
        }
        let mut out = String::with_capacity(self.raw.len());
        let mut cursor = 0;
        for slot in &self.slots {
            out.push_str(&self.raw[cursor..slot.span.start]);
            if slot.value.is_empty() {
                out.push_str(&self.raw[slot.span.clone()]);
            } else {
                out.push_str(&slot.value);
            }
            cursor = slot.span.end;
        }
        out.push_str(&self.raw[cursor..]);
        out
    }

    /// True when every slot has a non-empty value.
    pub fn is_complete(&self) -> bool {
        self.slots.iter().all(|s| !s.value.is_empty())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("unclosed placeholder at byte {0}")]
    Unclosed(usize),
    #[error("empty placeholder at byte {0}")]
    EmptyName(usize),
    #[error("invalid placeholder name at byte {0}")]
    InvalidName(usize),
    #[error("nested placeholder at byte {0}")]
    Nested(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fills_advances_and_renders_placeholders() {
        let mut tpl = Template::parse("rg {{pattern}} {{dir}}").unwrap();
        assert!(tpl.has_slots());
        assert_eq!(tpl.active_slot().unwrap().name, "pattern");
        assert!(!tpl.is_complete());
        assert_eq!(tpl.render(), "rg {{pattern}} {{dir}}");

        tpl.fill_active("TODO");
        assert_eq!(tpl.render(), "rg TODO {{dir}}");
        assert!(tpl.focus_next());
        assert_eq!(tpl.active_slot().unwrap().name, "dir");
        tpl.push_active_char('s');
        tpl.push_active_char('r');
        tpl.push_active_char('c');
        assert!(!tpl.focus_next());
        assert!(tpl.is_complete());
        assert_eq!(tpl.render(), "rg TODO src");
    }

    #[test]
    fn rejects_malformed_placeholders() {
        assert!(matches!(
            Template::parse("echo {{unclosed"),
            Err(TemplateError::Unclosed(_))
        ));
        assert!(matches!(
            Template::parse("echo {{}}"),
            Err(TemplateError::EmptyName(_))
        ));
        assert!(matches!(
            Template::parse("echo {{bad name}}"),
            Err(TemplateError::InvalidName(_))
        ));
        assert!(matches!(
            Template::parse("echo {{a{{b}}}}"),
            Err(TemplateError::Nested(_))
        ));
    }
}
