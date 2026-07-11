# Handoff: changes needed in rusty_libc

Audience: whoever works on https://github.com/baileyrd/rusty_libc next.
Context: rush's differential passes against bash 5.2 (C1–C135 in
`docs/CAPABILITY_GAPS.md`) are essentially complete on the rush side. The
one item below is blocked on a rusty_libc addition. rush pins rusty_libc by
git rev in `Cargo.toml`, so the flow is: land it there, bump the pin, then
close the rush side.

rush reaches rusty_libc only through `src/sys.rs`, which wraps the crate
behind a backend-agnostic surface (the rusty-libc backend is the Linux
default; a `libc`-crate backend covers other Unix and Linux-with-
`libc-backend`). Everything below is one constant on the rusty-libc side.

## 1. `RLIMIT_RTTIME` constant (blocks `ulimit -a`'s `-R` line) — ✅ DONE

**Landed:** rusty_libc now ships `RLIMIT_RTTIME = 15` (rev `65f467f`), and
the rush side is wired up — the pin is bumped, `sys.rs` re-exports the
constant, and `ULIMIT_RESOURCES` has the leading `-R` row, so `ulimit -a`'s
first line and `ulimit -R` get/set now match bash. The original write-up is
kept below for reference.

**Today:** `src/rlimit.rs` defines the `RLIMIT_*` identifiers up to
`RLIMIT_RTPRIO = 14` and stops there. bash 5.2's `ulimit -a` lists one more
resource rush therefore cannot report:

```
real-time non-blocking time  (microseconds, -R) unlimited
```

`ulimit -R` (the maximum microseconds a process may run under a real-time
scheduling policy without a blocking syscall) maps to `RLIMIT_RTTIME`, the
next asm-generic id after `RLIMIT_RTPRIO`. It's a plain `prlimit64`
resource — no new syscall, struct, or code path, just the missing id.

**Needed:** one constant, mirroring the existing ones:

```rust
/// Ceiling on real-time CPU time consumed without a blocking syscall,
/// in microseconds.
pub const RLIMIT_RTTIME: i32 = 15;
```

**rush integration point (after the pin bump):** two small edits, both
already scaffolded for the other resources.

1. `src/sys.rs` — add `RLIMIT_RTTIME` to the `pub use rusty_libc::rlimit::{…}`
   re-export in the rusty-libc backend (and it comes for free from the
   `libc` crate in the other backend, which already re-exports `libc::*`
   RLIMIT names).
2. `src/builtins.rs` — add one row to the `ULIMIT_RESOURCES` table, in
   bash's alphabetical-by-letter order (uppercase `-R` sorts first, so it
   becomes the first entry):

   ```rust
   #[cfg(any(target_os = "linux", target_os = "android"))]
   UlimitResource {
       letter: 'R',
       resource: crate::sys::RLIMIT_RTTIME as i32,
       label: "real-time non-blocking time  (microseconds, -R)",
       scale: 1,
   },
   ```

   That closes both `ulimit -a`'s missing row and `ulimit -R` get/set. The
   existing `ulimit` machinery (getrlimit/setrlimit over the shared table,
   the `unlimited` keyword, `-S`/`-H`) needs no other change.

## Explicitly *not* needed from rusty_libc

- **`ulimit -p` (pipe size).** bash reports `pipe size (512 bytes, -p) 8`,
  but this is a *pseudo-resource*: there is no `RLIMIT_PIPE`, and bash
  hardcodes the value from the atomic pipe-buffer size. It is not backed by
  `getrlimit`, so it needs no rusty_libc support — if rush ever wants the
  row, it's a rush-side synthetic entry, not a crate addition.
- **Everything else rush touches** (`fork`/`waitpid`/`fcntl`/`pipe`/
  `getrlimit`/`setrlimit`/`umask`/signal handling/`memfd_create`/`poll`/
  termios/`terminal_size`) is already present and in use. The `poll(2)`
  wrapper added for `read -t 0` uses `rusty_libc::fd::poll` + `PollFd` +
  `POLLIN`, all already shipped — no addition required there.

This is the only rusty_libc-blocked gap; it's a one-line constant plus the
two rush-side edits noted above. Low priority — a single cosmetic line in
`ulimit -a` output — but self-contained and quick.
