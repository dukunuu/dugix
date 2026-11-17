//! What an agent is doing, read off its screen: the CLIs publish no state, so
//! this looks for the prompts and progress lines they draw, and falls back to
//! whether anything was printed lately.

use std::time::Duration;

use super::State;

/// Output newer than this means the agent is still working.
pub(super) const QUIET: Duration = Duration::from_millis(1500);
/// A question on screen counts once the screen has settled for this long.
const SETTLED: Duration = Duration::from_millis(300);
/// Lines from the bottom of the screen that are searched: prompts and progress
/// lines are drawn there, and older transcript text above must not match.
const TAIL_LINES: usize = 14;

/// Drawn while the agent waits for a permission or trust decision.
const BLOCKED: &[&str] = &[
    "do you want to",
    "do you trust",
    "would you like to",
    "allow once",
    "allow always",
    "permission required",
    "yes, proceed",
    "enter to confirm",
    "(y/n)",
    "[y/n]",
];

/// Drawn while a turn is running.
const WORKING: &[&str] = &["esc to interrupt", "esc interrupt", "ctrl+c to interrupt"];

/// The last non-empty lines of a screen, lowercased.
fn tail(screen: &str) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..]
        .join("\n")
        .to_lowercase()
}

/// The state of a running agent whose screen shows `screen` and that last
/// printed `quiet` ago.
pub(super) fn classify(screen: &str, quiet: Duration) -> State {
    let tail = tail(screen);
    if quiet >= SETTLED && BLOCKED.iter().any(|marker| tail.contains(marker)) {
        State::Blocked
    } else if quiet < QUIET || WORKING.iter().any(|marker| tail.contains(marker)) {
        State::Working
    } else {
        State::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(10);

    #[test]
    fn a_permission_prompt_is_blocked_once_the_screen_settles() {
        let screen = "Bash command\n  rm -rf build\nDo you want to proceed?\n❯ 1. Yes\n  2. No";
        assert_eq!(classify(screen, LONG), State::Blocked);
        assert_eq!(
            classify(screen, Duration::from_millis(50)),
            State::Working,
            "still being drawn"
        );
    }

    #[test]
    fn a_progress_line_is_working_however_quiet() {
        assert_eq!(
            classify("✻ Pondering… (12s · esc to interrupt)", LONG),
            State::Working
        );
    }

    #[test]
    fn a_settled_screen_without_markers_is_idle() {
        assert_eq!(classify("Done.\n\n> ", LONG), State::Idle);
        assert_eq!(classify("Done.\n\n> ", Duration::ZERO), State::Working);
    }

    #[test]
    fn a_question_scrolled_far_above_the_prompt_no_longer_blocks() {
        let mut screen = String::from("Do you want to proceed?\n");
        for line in 0..TAIL_LINES {
            screen.push_str(&format!("output line {line}\n"));
        }
        assert_eq!(classify(&screen, LONG), State::Idle);
    }
}
