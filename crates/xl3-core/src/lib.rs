//! xl3-core — pure-Rust XLSX template rendering engine.
//!
//! Phase 0 status: scaffolding only. The pipeline modules
//! (source / plan / eval / output / render) land in Phase 1 — see
//! `PLAN.md` §4.3 and §5.
//!
//! Re-exports the underlying engines so the Phase 0 measurement example
//! (and any future in-tree consumers) cannot drift onto a different
//! version than the crate itself.

pub use calamine;
pub use rust_xlsxwriter;
