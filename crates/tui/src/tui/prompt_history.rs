//! Session-scoped literal prompt history for the fullscreen composer.

pub const MAX_PROMPT_HISTORY_ENTRIES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftSnapshot {
    buffer: String,
    cursor_char_idx: usize,
}

/// Small state machine for recalling submitted composer drafts.
///
/// `cursor` is `None` outside history navigation. While navigating, `draft`
/// retains the exact buffer and cursor that existed before the first recall so
/// moving down past the newest entry can restore it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: Option<DraftSnapshot>,
}

impl PromptHistory {
    pub fn new(entries: impl IntoIterator<Item = String>) -> Self {
        let mut history = Self::default();
        history.replace(entries);
        history
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn is_navigating(&self) -> bool {
        self.cursor.is_some()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.detach();
    }

    pub fn replace(&mut self, entries: impl IntoIterator<Item = String>) {
        self.entries = entries
            .into_iter()
            .filter(|entry| !entry.trim().is_empty())
            .collect();
        if self.entries.len() > MAX_PROMPT_HISTORY_ENTRIES {
            let first = self.entries.len() - MAX_PROMPT_HISTORY_ENTRIES;
            self.entries.drain(..first);
        }
        self.detach();
    }

    pub fn record(&mut self, entry: &str) -> bool {
        if entry.trim().is_empty() {
            return false;
        }
        self.entries.push(entry.to_string());
        if self.entries.len() > MAX_PROMPT_HISTORY_ENTRIES {
            self.entries.remove(0);
        }
        self.detach();
        true
    }

    /// Recall the newest entry, or move to the next older entry.
    pub fn previous(&mut self, buffer: &str, cursor_char_idx: usize) -> Option<(String, usize)> {
        if self.entries.is_empty() {
            return None;
        }
        let index = match self.cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = Some(DraftSnapshot {
                    buffer: buffer.to_string(),
                    cursor_char_idx: cursor_char_idx.min(buffer.chars().count()),
                });
                self.entries.len() - 1
            }
        };
        self.cursor = Some(index);
        Some(self.entry_at(index))
    }

    /// Recall the next newer entry, or restore the pre-navigation draft.
    pub fn next_prompt(&mut self) -> Option<(String, usize)> {
        let index = self.cursor?;
        if index + 1 < self.entries.len() {
            let next = index + 1;
            self.cursor = Some(next);
            return Some(self.entry_at(next));
        }

        let draft = self.draft.take()?;
        self.cursor = None;
        Some((draft.buffer, draft.cursor_char_idx))
    }

    /// Leave history navigation while preserving the current composer buffer.
    pub fn detach(&mut self) {
        self.cursor = None;
        self.draft = None;
    }

    fn entry_at(&self, index: usize) -> (String, usize) {
        let entry = self.entries[index].clone();
        let cursor = entry.chars().count();
        (entry, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traverses_newest_to_oldest_and_restores_scratch() {
        let mut history = PromptHistory::new(["old".into(), "new".into()]);
        assert_eq!(history.previous("unfinished", 3), Some(("new".into(), 3)));
        assert!(history.is_navigating());
        assert_eq!(history.previous("new", 3), Some(("old".into(), 3)));
        assert_eq!(history.next_prompt(), Some(("new".into(), 3)));
        assert_eq!(history.next_prompt(), Some(("unfinished".into(), 3)));
        assert!(!history.is_navigating());
    }

    #[test]
    fn preserves_exact_unicode_multiline_and_duplicates() {
        let value = "  café\n\n終わり  \n";
        let mut history = PromptHistory::new([value.into(), value.into()]);
        assert_eq!(
            history.previous("", 0),
            Some((value.into(), value.chars().count()))
        );
        assert_eq!(
            history.previous(value, value.chars().count()),
            Some((value.into(), value.chars().count()))
        );
        assert_eq!(history.entries(), &[value.to_string(), value.to_string()]);
    }

    #[test]
    fn ignores_blank_entries_and_caps_at_one_hundred() {
        let mut history = PromptHistory::new(std::iter::once(" ".into()));
        assert!(history.entries().is_empty());
        for index in 0..101 {
            history.record(&index.to_string());
        }
        assert_eq!(history.entries().len(), MAX_PROMPT_HISTORY_ENTRIES);
        assert_eq!(history.entries().first().map(String::as_str), Some("1"));
        assert!(!history.record("\n\t"));
    }

    #[test]
    fn detach_preserves_current_navigation_buffer_without_restoring_scratch() {
        let mut history = PromptHistory::new(["one".into()]);
        assert_eq!(history.previous("scratch", 2), Some(("one".into(), 3)));
        history.detach();
        assert!(!history.is_navigating());
        assert_eq!(history.next_prompt(), None);
    }

    #[test]
    fn empty_history_and_boundaries_preserve_the_current_draft() {
        let mut empty = PromptHistory::default();
        assert_eq!(empty.previous("scratch", 3), None);
        assert_eq!(empty.next_prompt(), None);

        let mut history = PromptHistory::new(["only".into()]);
        assert_eq!(history.previous("scratch", 3), Some(("only".into(), 4)));
        assert_eq!(history.previous("only", 4), Some(("only".into(), 4)));
        assert_eq!(history.next_prompt(), Some(("scratch".into(), 3)));
        assert_eq!(history.next_prompt(), None);
    }
}
