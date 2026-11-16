//! UI modules: widget kit and screens.

pub mod connform;
pub mod connlist;
pub mod grid;
pub mod console;
pub mod datatab;
pub mod palette;
pub mod quicklook;
pub mod sidebar;
pub mod settings;
pub mod tabs;
pub mod workspace;
pub mod widgets;

use std::time::{Duration, Instant};

/// How long a toast stays visible.
const TOAST_SECS: u64 = 3;
/// How long an error toast stays visible.
const TOAST_ERROR_SECS: u64 = 6;

/// Kind of a toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Ok,
    Info,
    Error,
}

/// One notification.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: Instant,
}

/// Queue of toasts shown bottom-right.
#[derive(Debug, Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
}

impl Toasts {
    /// Success toast.
    pub fn ok(&mut self, text: impl Into<String>) {
        self.push(text.into(), ToastKind::Ok);
    }

    /// Information toast.
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(text.into(), ToastKind::Info);
    }

    /// Error toast (stays longer).
    pub fn err(&mut self, text: impl Into<String>) {
        self.push(text.into(), ToastKind::Error);
    }

    /// Adds a toast.
    fn push(&mut self, text: String, kind: ToastKind) {
        self.items.push(Toast { text, kind, born: Instant::now() });
    }

    /// Lifetime of a toast kind.
    pub fn lifetime(kind: ToastKind) -> Duration {
        Duration::from_secs(if kind == ToastKind::Error { TOAST_ERROR_SECS } else { TOAST_SECS })
    }

    /// Drops expired toasts.
    pub fn prune(&mut self) {
        self.items.retain(|t| t.born.elapsed() < Self::lifetime(t.kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Errors outlive other toasts, and pruning removes only expired ones.
    #[test]
    fn toast_lifetimes_and_pruning() {
        assert!(Toasts::lifetime(ToastKind::Error) > Toasts::lifetime(ToastKind::Ok));
        let mut t = Toasts::default();
        t.ok("saved");
        t.err("failed");
        t.items[0].born = Instant::now() - Duration::from_secs(TOAST_SECS + 1);
        t.items[1].born = Instant::now() - Duration::from_secs(TOAST_SECS + 1);
        t.prune();
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].kind, ToastKind::Error);
    }
}
