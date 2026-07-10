//! Alias table: `alias name=value` substitutes `name` for `value`'s
//! whitespace-split words at the start of a simple command (`expand.rs`'s
//! `expand_simple`), before it's resolved as a function/builtin/external
//! program. A single, non-recursive substitution — the expanded words aren't
//! re-checked against the alias table, so `alias ls='ls --color=auto'`
//! doesn't self-recurse.

use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static ALIASES: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
    // Abbreviations (C70): fish/zsh-style names that live-expand on the
    // interactive line itself (on space, in command position) — kept
    // separate from aliases, as fish and zsh both do, since the two
    // expand at completely different times.
    static ABBRS: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
}

pub fn abbr_set(name: &str, value: &str) {
    ABBRS.with(|a| a.borrow_mut().insert(name.to_string(), value.to_string()));
}

pub fn abbr_get(name: &str) -> Option<String> {
    ABBRS.with(|a| a.borrow().get(name).cloned())
}

/// Removes `name`; returns whether it was actually defined.
pub fn abbr_unset(name: &str) -> bool {
    ABBRS.with(|a| a.borrow_mut().remove(name).is_some())
}

/// All abbreviations, name-sorted.
pub fn abbr_all() -> Vec<(String, String)> {
    ABBRS.with(|a| a.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

pub fn set(name: &str, value: &str) {
    ALIASES.with(|a| a.borrow_mut().insert(name.to_string(), value.to_string()));
}

pub fn get(name: &str) -> Option<String> {
    ALIASES.with(|a| a.borrow().get(name).cloned())
}

/// Removes `name`; returns whether it was actually defined.
pub fn unset(name: &str) -> bool {
    ALIASES.with(|a| a.borrow_mut().remove(name).is_some())
}

pub fn unset_all() {
    ALIASES.with(|a| a.borrow_mut().clear());
}

/// All aliases, name-sorted (a `BTreeMap` iterates in key order already).
pub fn all() -> Vec<(String, String)> {
    ALIASES.with(|a| a.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_unset() {
        set("ll", "ls -l");
        assert_eq!(get("ll").as_deref(), Some("ls -l"));
        assert!(unset("ll"));
        assert_eq!(get("ll"), None);
        assert!(!unset("ll"));
    }
}
