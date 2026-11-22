//! Helpers for writing days-of-our-lives plugins.
//!
//! The protocol (newline-delimited JSON over stdin/stdout) is defined in [`protocol`];
//! the bar uses the same types. Plugins normally implement [`Plugin`] and call [`run`].

pub mod protocol;
mod run;
pub mod svg;
pub mod tick;
#[cfg(feature = "hypr")]
pub mod hypr;

pub use protocol::{Button, Init, Pointer, Rect, Style};
pub use run::{run, Describe, Frame, Mouse, Out, Plugin, Tx};
