//! The command-line reader lives in `model::shell` since the write gate
//! (`model::store`) has to count command-anchor rivals with the very same
//! "this anchor reaches this call" definition the runtime serves by, and
//! `model` cannot depend on `serve`. Everything is re-exported here so every
//! `shell::...` path in this crate is unchanged; the tests stay beside the
//! readers that need `absent_guard`.

pub use model::shell::*;

#[cfg(test)]
mod tests;
