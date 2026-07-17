//! rush's library crate.
//!
//! rush is primarily a binary (`src/main.rs`); this `lib.rs` exists so two
//! other things can link against its internals directly instead of going
//! through a subprocess: the `fuzz/` targets (`lexer::lex`/`parser::parse`/
//! `arith::eval` need in-process, coverage-guided fuzzing — spawning a real
//! process per input would lose most of libFuzzer's value) and `benches/`
//! (same reasoning, for throughput measurement instead of coverage).
//! `src/main.rs` also depends on this crate for its own module tree, via a
//! `use rush::*;` glob import — every module here is `pub` for that reason,
//! not because this is meant as a general-purpose external API. Module
//! boundaries and internal invariants are exactly as `ARCHITECTURE.md`
//! describes; nothing changed by this split.

pub mod alias;
pub mod arith;
pub mod builtins;
pub mod completion;
pub mod exec;
pub mod expand;
pub mod func;
pub mod glob;
pub mod history_expand;
#[cfg(unix)]
pub mod job;
pub mod lexer;
pub mod parser;
#[cfg(unix)]
pub mod sys;
pub mod trap;
pub mod unparse;
pub mod vars;
#[cfg(not(unix))]
pub mod winstdio;
