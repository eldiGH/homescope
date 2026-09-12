//! Consent gates for the paths that destroy something.
//!
//! One rule decides where these go: **prompt exactly when a step destroys
//! something the tool can see, or something it cannot see and therefore cannot
//! rule out.** Everything else runs silent, so the happy path stays
//! prompt-free and a prompt keeps meaning something.
//!
//! Hand-rolled rather than `dialoguer`, because the part that matters — the
//! TTY policy and failing closed — is a decision no crate makes for you.

use std::io::{BufRead as _, IsTerminal as _, Write as _, stderr, stdin};

use thiserror::Error;

/// Asks a yes/no question, defaulting to **no**.
///
/// Used where the tool can name what it is about to destroy; the identity
/// block printed immediately above is what actually catches a wrong board, and
/// the keystroke only records that you read it.
pub fn yes_no(question: &str, assume_yes: bool) -> Result<(), ConfirmError> {
    match ask(question, "[y/N]", assume_yes)? {
        None => Ok(()),
        Some(answer) => decide_yes_no(&answer),
    }
}

/// ⚠️ Exact matches only — no prefix acceptance. "yeah" is not a yes, because
/// anything looser turns a typo into consent at a gate that destroys a key.
fn decide_yes_no(answer: &str) -> Result<(), ConfirmError> {
    match answer.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(ConfirmError::Aborted),
    }
}

/// Demands that an exact word be typed back.
///
/// For blind consent — where APPROTECT means the tool cannot read, and so
/// cannot name, the board it is about to wipe. It is a weak guard, but it
/// costs a deliberate act rather than a reflex.
pub fn typed(question: &str, expected: &str, assume_yes: bool) -> Result<(), ConfirmError> {
    match ask(
        question,
        &format!("(type {expected} to continue)"),
        assume_yes,
    )? {
        None => Ok(()),
        Some(answer) => decide_typed(&answer, expected),
    }
}

/// Case-sensitive and exact. The whole point is that it cannot be answered by
/// reflex, so `erase` does not stand in for `ERASE`.
fn decide_typed(answer: &str, expected: &str) -> Result<(), ConfirmError> {
    if answer == expected {
        Ok(())
    } else {
        Err(ConfirmError::Aborted)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Policy {
    /// `--yes` already answered.
    Granted,
    Ask,
    /// ⚠️ Fail closed: no terminal and no `--yes` is an error naming the flag,
    /// never an implicit yes. Split out of the IO so this cannot be verified
    /// only by hand.
    Refuse,
}

fn policy(assume_yes: bool, stdin_is_terminal: bool) -> Policy {
    match (assume_yes, stdin_is_terminal) {
        (true, _) => Policy::Granted,
        (false, true) => Policy::Ask,
        (false, false) => Policy::Refuse,
    }
}

/// `Ok(None)` means `--yes` already answered.
///
/// ⚠️ Prompt and answer live on stderr/stdin, never stdout — stdout carries the
/// one durable fact a command produces, so it stays redirectable.
fn ask(question: &str, suffix: &str, assume_yes: bool) -> Result<Option<String>, ConfirmError> {
    match policy(assume_yes, stdin().is_terminal()) {
        Policy::Granted => return Ok(None),
        Policy::Refuse => return Err(ConfirmError::NoTty),
        Policy::Ask => {}
    }

    write!(stderr(), "\n{question} {suffix} ")?;
    stderr().flush()?;

    let mut answer = String::new();
    stdin().lock().read_line(&mut answer)?;

    // A closed stdin reads as empty, which falls through to the default — no.
    Ok(Some(answer.trim().to_owned()))
}

/// ⚠️ Returned instead of a `bool` deliberately. A `bool` at a safety gate can
/// be called and its answer dropped: we have been bitten by exactly that shape
/// before, when a `verify_words(…)?` returning a discarded `bool` was a
/// verification step that verified nothing. A `?` cannot be ignored, and
/// `#[must_use]` is only the weaker version of the same idea.
#[derive(Debug, Error)]
pub enum ConfirmError {
    #[error("aborted")]
    Aborted,

    #[error(
        "cannot ask for confirmation: stdin is not a terminal\n\npass --yes to proceed without prompting"
    )]
    NoTty,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod test {
    use super::*;

    fn aborted(result: Result<(), ConfirmError>) -> bool {
        matches!(result, Err(ConfirmError::Aborted))
    }

    #[test]
    fn yes_flag_grants_regardless_of_the_terminal() {
        assert_eq!(policy(true, true), Policy::Granted);
        assert_eq!(policy(true, false), Policy::Granted);
    }

    #[test]
    fn a_terminal_without_the_flag_is_asked() {
        assert_eq!(policy(false, true), Policy::Ask);
    }

    /// ⚠️ The one that matters. Without a terminal there is nobody to ask, and
    /// the answer must be no — never an implicit yes because a pipe happened to
    /// carry the word "y".
    #[test]
    fn no_terminal_and_no_flag_fails_closed() {
        assert_eq!(policy(false, false), Policy::Refuse);
    }

    /// `--yes` short-circuits before any IO, so this exercises the real entry
    /// points without needing a pty.
    #[test]
    fn assume_yes_skips_the_prompt_entirely() {
        assert!(yes_no("destroy it?", true).is_ok());
        assert!(typed("erase it?", "ERASE", true).is_ok());
    }

    #[test]
    fn yes_is_accepted_in_any_case() {
        for answer in ["y", "Y", "yes", "YES", "Yes", "yEs"] {
            assert!(decide_yes_no(answer).is_ok(), "{answer:?} should be a yes");
        }
    }

    #[test]
    fn an_empty_answer_defaults_to_no() {
        // Plain Enter, and a closed stdin, both arrive here as "".
        assert!(aborted(decide_yes_no("")));
    }

    /// ⚠️ Nothing may be accepted by prefix. "yeah" reads like agreement and is
    /// not one; accepting it would turn a typo into a destroyed key.
    #[test]
    fn near_misses_are_not_a_yes() {
        for answer in [
            "n", "N", "no", "nope", "yeah", "ya", "yep", "ok", "sure", "1",
        ] {
            assert!(aborted(decide_yes_no(answer)), "{answer:?} should abort");
        }
    }

    #[test]
    fn typed_requires_the_exact_word() {
        assert!(decide_typed("ERASE", "ERASE").is_ok());
    }

    #[test]
    fn typed_is_case_sensitive_and_rejects_near_misses() {
        for answer in ["erase", "Erase", "ERASED", "ERAS", "", "y"] {
            assert!(
                aborted(decide_typed(answer, "ERASE")),
                "{answer:?} should abort"
            );
        }
    }

    /// The refusal has to name the way out, or the operator is stuck.
    #[test]
    fn the_no_terminal_error_names_the_flag() {
        assert!(ConfirmError::NoTty.to_string().contains("--yes"));
    }
}
