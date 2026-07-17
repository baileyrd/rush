//! Fuzzes `arith::eval` — the `$((...))`/`((...))` evaluator. History here
//! is exactly why this target exists: `$((MAX+1))` once panicked the whole
//! shell (integer overflow, fixed by switching every op to `wrapping_*` —
//! see docs/CAPABILITY_GAPS.md, C132) and was found by manual differential
//! testing against bash, not fuzzing. A division/modulo-by-zero, a deeply
//! nested `(((((...)))))`, or a malformed ternary are the kind of input
//! this target is aimed at catching before a user does.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let _ = rush::arith::eval(input);
});
