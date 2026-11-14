//! Catch a panic from third-party code (a parser fed a malformed file) and turn
//! it into an error. While a guarded call runs, the thread is marked so the
//! TUI's panic hook neither prints over the screen nor restores the terminal.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
    static CATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is inside [`catch`]; a panic here will be recovered.
#[must_use]
pub fn catching() -> bool {
    CATCHING.get()
}

/// Run `f`, returning its panic message as an error instead of unwinding.
pub fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let previous = CATCHING.replace(true);
    let result = catch_unwind(AssertUnwindSafe(f));
    CATCHING.set(previous);
    result.map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(ToString::to_string)
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_its_message_and_the_mark_is_scoped() {
        assert!(!catching());
        let error = catch(|| -> u8 {
            assert!(catching());
            panic!("index out of bounds: the len is 0 but the index is 0");
        })
        .unwrap_err();
        assert!(error.contains("index out of bounds"), "{error}");
        assert!(!catching());
        assert_eq!(catch(|| 7), Ok(7));
        let nested = catch(|| catch(|| catching()).unwrap() && catching());
        assert_eq!(nested, Ok(true));
        assert!(!catching());
    }
}
