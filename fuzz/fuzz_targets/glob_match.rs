//! Fuzzes `glob::match_component` — the hand-rolled matcher backing
//! filename globbing, `case`, and the `${v#pat}`-family pattern-removal
//! operators (they all share this one matcher, per
//! docs/CAPABILITY_GAPS.md C42). Pure and filesystem-free, unlike
//! `glob::glob` itself, so safe to drive directly with arbitrary input.
//! The input is split on the first newline into a pattern and a name to
//! match it against; input with no newline matches a string against
//! itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let (pattern, name) = input.split_once('\n').unwrap_or((input, input));
    let _ = rush::glob::match_component(pattern, name);
});
