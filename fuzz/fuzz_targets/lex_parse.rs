//! Fuzzes the lex→parse pipeline (`lexer::lex` is called internally by
//! `parser::parse`) — rush's own hand-rolled recursive-descent parser over
//! a hand-rolled lexer, exactly the kind of component most likely to panic
//! on adversarial input rather than return a clean `Err`. Deliberately
//! stops at parsing: it never hands the result to `expand`/`exec`, since
//! that stage can run real command substitutions and glob against the
//! filesystem — not what a fuzzer should be doing with arbitrary input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let _ = rush::parser::parse(input);
});
