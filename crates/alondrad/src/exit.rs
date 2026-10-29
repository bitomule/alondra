use std::sync::Once;

/// Ends the daemon while other threads run. Two threads calling `std::process::exit` at once is
/// undefined behaviour on Unix (C's `exit` is not thread-safe), so the first caller says its
/// `last_words` and exits, and any other waits here until the process is gone.
pub(crate) fn now(code: i32, last_words: impl FnOnce()) -> ! {
    static EXITING: Once = Once::new();
    EXITING.call_once(|| {
        last_words();
        std::process::exit(code)
    });
    unreachable!("the first caller of exit::now ends the process")
}
