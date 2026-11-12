//! Inline command staging/confirmation state: placeholder editing and safety gate.
//! All rendering happens inline inside `grid.rs` in the terminal's monospace font (no popup windows).

use crate::contracts::{Candidate, CandidateSource, CwdContext, SafetyReport};
use crate::input::placeholder::Template;
use crate::input::safety::{self, SafetyPolicy};

/// One input event consumed by the overlay while it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    MoveUp,
    MoveDown,
    /// Tab: fill/advance the active placeholder.
    NextSlot,
    Backspace,
    Char(char),
    /// Enter: stage through `ConfirmGate`.
    #[allow(dead_code)] // Shortcut: Confirm уходит в app отдельной веткой.
    Confirm,
    /// Esc: close and discard.
    Cancel,
}

/// All overlay state lives here so `app` stays a pure router.
#[derive(Clone, Debug, Default)]
pub struct OverlayState {
    pub open: bool,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    /// Present when the selected template has `{{arg}}` slots.
    pub template: Option<Template>,
    pub error: Option<String>,
}

impl OverlayState {
    /// `error` is the only place provider failures are surfaced; no panics.
    pub fn with_candidates(candidates: Vec<Candidate>, error: Option<String>) -> Self {
        let mut state = Self {
            open: true,
            candidates,
            selected: 0,
            template: None,
            error,
        };
        state.sync_template();
        state
    }

    fn sync_template(&mut self) {
        self.template = self
            .candidates
            .get(self.selected)
            .and_then(|c| Template::parse(&c.template).ok())
            .filter(|t| t.has_slots());
        self.sync_rendered_from_template();
    }

    fn sync_rendered_from_template(&mut self) {
        if let (Some(tpl), Some(cand)) = (&self.template, self.candidates.get_mut(self.selected)) {
            cand.rendered = tpl.render();
        }
    }

    pub fn reanalyze_selected(&mut self, policy: &SafetyPolicy, ctx: &CwdContext) {
        if let Some(cand) = self.candidates.get_mut(self.selected) {
            cand.safety = safety::analyze(&cand.rendered, policy, ctx);
        }
    }

    pub fn selected_candidate(&self) -> Option<&Candidate> {
        self.candidates.get(self.selected)
    }

    /// Keeps the preview in sync when selection changes; rebuilds the template.
    pub fn select(&mut self, index: usize) {
        if self.candidates.is_empty() {
            self.selected = 0;
            self.template = None;
        } else {
            self.selected = index.min(self.candidates.len() - 1);
            self.sync_template();
        }
    }

    /// Applies one event. Returns `true` when the overlay should close.
    pub fn handle(&mut self, event: OverlayEvent) -> bool {
        match event {
            OverlayEvent::MoveUp => {
                if !self.candidates.is_empty() {
                    let next = if self.selected == 0 {
                        self.candidates.len() - 1
                    } else {
                        self.selected - 1
                    };
                    self.select(next);
                }
                false
            }
            OverlayEvent::MoveDown => {
                if !self.candidates.is_empty() {
                    let next = (self.selected + 1) % self.candidates.len();
                    self.select(next);
                }
                false
            }
            OverlayEvent::NextSlot => {
                if let Some(tpl) = self.template.as_mut() {
                    if !tpl.focus_next() && !tpl.slots.is_empty() {
                        tpl.active = 0;
                    }
                }
                false
            }
            OverlayEvent::Backspace => {
                if let Some(tpl) = self.template.as_mut() {
                    tpl.pop_active_char();
                    self.sync_rendered_from_template();
                }
                false
            }
            OverlayEvent::Char(ch) => {
                if let Some(tpl) = self.template.as_mut() {
                    tpl.push_active_char(ch);
                    self.sync_rendered_from_template();
                }
                false
            }
            OverlayEvent::Cancel => {
                self.open = false;
                self.candidates.clear();
                self.selected = 0;
                self.template = None;
                self.error = None;
                true
            }
            OverlayEvent::Confirm => true,
        }
    }

    /// Rendered command for the current selection, with placeholder values applied.
    pub fn preview(&self) -> Option<String> {
        if let Some(tpl) = &self.template {
            return Some(tpl.render());
        }
        self.selected_candidate().map(|c| c.rendered.clone())
    }

    pub fn risk(&self) -> SafetyReport {
        self.selected_candidate()
            .map(|c| c.safety.clone())
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn source_label(&self) -> Option<CandidateSource> {
        self.selected_candidate().map(|c| c.source)
    }
}
