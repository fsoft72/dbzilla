//! Vim emulation for the SQL editor: a rope buffer with undo, selection state and the modal
//! key handling (ported from the fed editor). No UI code lives here.

pub mod buffer;
pub mod editing;
pub mod search;
pub mod text_nav;
pub mod vim;

pub use vim::{Mode, VimEffect, VimKey, VimState};
