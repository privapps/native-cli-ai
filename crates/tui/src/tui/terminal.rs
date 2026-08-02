//! Terminal setup/teardown helpers.
//!
//! Extracted from `tui/app.rs` in Phase 2.2.

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout, Write, stdout};

fn request_keyboard_enhancement(out: &mut impl Write) -> io::Result<()> {
    match execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    ) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        // Ask supporting terminals and multiplexers to report modified keys
        // using CSI-u. Without this, Shift+Enter is commonly indistinguishable
        // from ordinary Enter at the byte stream level.
        request_keyboard_enhancement(&mut out)?;
        execute!(out, EnableMouseCapture)?;
        execute!(out, EnableBracketedPaste)?;
        execute!(out, Hide)?;
        execute!(out, Clear(ClearType::All))?;
        Ok(Terminal::new(CrosstermBackend::new(out))?)
    })();
    if res.is_err() {
        // Initialization can fail after alternate-screen, mouse capture, or
        // bracketed paste has already been enabled. Use the same complete
        // cleanup path as the normal session teardown.
        restore_terminal();
    }
    res
}

/// Restores terminal state when a full-screen session exits through an error.
///
/// The guard is deliberately independent of ratatui's `Terminal`: crossterm
/// input and drawing errors can happen at any point after setup succeeds, and
/// every one of those paths must disable bracketed paste before returning.
pub struct TerminalRestoreGuard<F: FnOnce() = fn()> {
    cleanup: Option<F>,
}

impl TerminalRestoreGuard<fn()> {
    pub fn new() -> Self {
        Self {
            cleanup: Some(restore_terminal),
        }
    }
}

impl Default for TerminalRestoreGuard<fn()> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: FnOnce()> TerminalRestoreGuard<F> {
    #[cfg(test)]
    pub(crate) fn with_cleanup(cleanup: F) -> Self {
        Self {
            cleanup: Some(cleanup),
        }
    }

    pub fn disarm(&mut self) {
        self.cleanup = None;
    }
}

impl<F: FnOnce()> Drop for TerminalRestoreGuard<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

pub fn restore_terminal() {
    let mut out = stdout();
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableMouseCapture);
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::{TerminalRestoreGuard, request_keyboard_enhancement};
    use std::cell::Cell;
    use std::io::{self, Write};
    use std::rc::Rc;

    #[test]
    fn restore_guard_runs_cleanup_on_drop() {
        let cleaned_up = Rc::new(Cell::new(false));
        {
            let marker = Rc::clone(&cleaned_up);
            let _guard = TerminalRestoreGuard::with_cleanup(move || marker.set(true));
        }
        assert!(cleaned_up.get());
    }

    #[test]
    fn restore_guard_can_be_disarmed_after_explicit_cleanup() {
        let cleanup_count = Rc::new(Cell::new(0));
        {
            let marker = Rc::clone(&cleanup_count);
            let mut guard =
                TerminalRestoreGuard::with_cleanup(move || marker.set(marker.get() + 1));
            guard.disarm();
        }
        assert_eq!(cleanup_count.get(), 0);
    }

    #[test]
    fn modified_key_setup_requests_csi_u_disambiguation() {
        let mut output = Vec::new();
        request_keyboard_enhancement(&mut output).expect("serialize keyboard enhancement request");

        assert_eq!(output, b"\x1b[>1u");
    }

    #[test]
    fn unsupported_keyboard_enhancement_is_optional() {
        struct UnsupportedWriter;

        impl Write for UnsupportedWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "keyboard enhancement is unavailable",
                ))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        request_keyboard_enhancement(&mut UnsupportedWriter)
            .expect("unsupported keyboard enhancement should be optional");
    }
}
