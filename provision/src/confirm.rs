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
    let Some(answer) = ask(question, "[y/N]", assume_yes)? else {
        return Ok(());
    };

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
    let Some(answer) = ask(
        question,
        &format!("(type {expected} to continue)"),
        assume_yes,
    )?
    else {
        return Ok(());
    };

    if answer == expected {
        Ok(())
    } else {
        Err(ConfirmError::Aborted)
    }
}

/// `Ok(None)` means `--yes` already answered.
///
/// ⚠️ Prompt and answer live on stderr/stdin, never stdout — stdout carries the
/// one durable fact a command produces, so it stays redirectable.
fn ask(question: &str, suffix: &str, assume_yes: bool) -> Result<Option<String>, ConfirmError> {
    if assume_yes {
        return Ok(None);
    }

    // ⚠️ Fail closed. No terminal and no --yes is an error naming the flag,
    // never an implicit yes.
    if !stdin().is_terminal() {
        return Err(ConfirmError::NoTty);
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
