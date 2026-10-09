//! UI of the noxfm windows.

pub mod browser;
pub mod clipboard;
pub mod daemon;
pub mod icons;
pub mod selection;
pub mod transfers;

// Shared with the GTK window (crates/noxfm-gtk).
pub use noxfm_core::{fmt, preview};
