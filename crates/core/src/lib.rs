//! SplitView's pure logic: no UI, no I/O. Everything here is unit-tested.
//!
//! * [`layout`] — the split tree (CLAUDE.md §7)
//! * [`keymap`] — the prefix engine and default bindings (§9)
//! * [`config`] — `config.toml` parsing and validation
//! * [`paths`], [`quote`] — path forms and shell quoting
//! * [`osc`] — picks OSC 7 / OSC 5170 out of the PTY stream
//! * [`tree`] — the file sidebar's rows
//! * [`editor`], [`highlight`] — the editor panel's model and syntax colouring
//! * [`fuzzy`] — command palette matching

pub mod config;
pub mod editor;
pub mod fuzzy;
pub mod highlight;
pub mod keymap;
pub mod layout;
pub mod osc;
pub mod paths;
pub mod quote;
pub mod tree;
