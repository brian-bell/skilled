//! SIGINT requests the same orderly shutdown as Ctrl-C input. The handler
//! performs only a lock-free atomic store; terminal and worker cleanup happen
//! on the event loop, never in signal context. Confirmed writes finish first.

#[cfg(unix)]
mod unix {
    use std::{
        io,
        sync::{
            Mutex, MutexGuard, TryLockError,
            atomic::{AtomicBool, Ordering},
        },
    };

    static INTERRUPTED: AtomicBool = AtomicBool::new(false);
    static OWNER: Mutex<()> = Mutex::new(());

    extern "C" fn request_interrupt(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::Relaxed);
    }

    pub(crate) struct InterruptHandler {
        previous: libc::sigaction,
        _owner: MutexGuard<'static, ()>,
    }

    impl InterruptHandler {
        pub(crate) fn install() -> io::Result<Self> {
            let owner = match OWNER.try_lock() {
                Ok(owner) => owner,
                // Unwinding restored the disposition in Drop. Poison does
                // not mean another terminal still owns the signal handler.
                Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::other(
                        "a terminal interrupt handler is already active",
                    ));
                }
            };
            INTERRUPTED.store(false, Ordering::Relaxed);
            // SAFETY: sigaction is a C record, initialized before use; both
            // pointers name live records. The callback has the signal ABI and
            // touches only a lock-free AtomicBool. No allocation, locks, I/O,
            // or unwinding occurs in signal context.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous = std::mem::zeroed();
                action.sa_sigaction = request_interrupt as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_flags = libc::SA_RESTART;
                if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self {
                    previous,
                    _owner: owner,
                })
            }
        }

        pub(crate) fn requested(&self) -> bool {
            INTERRUPTED.load(Ordering::Relaxed)
        }
    }

    impl Drop for InterruptHandler {
        fn drop(&mut self) {
            // SAFETY: restore the disposition captured by this exclusive
            // owner, after terminal cleanup, while the record is still live.
            unsafe {
                libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::InterruptHandler;

        #[test]
        fn a_caught_panic_does_not_prevent_a_later_terminal_session() {
            let panic = std::panic::catch_unwind(|| {
                let _handler = InterruptHandler::install().unwrap();
                assert!(
                    InterruptHandler::install().is_err(),
                    "a live owner must remain exclusive"
                );
                panic!("injected session unwind");
            });
            assert!(panic.is_err());
            let _next_session =
                InterruptHandler::install().expect("the prior owner restored SIGINT");
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::InterruptHandler;

// Windows is not a release target. Keep the existing Ctrl-C key path there.
#[cfg(not(unix))]
pub(crate) struct InterruptHandler;

#[cfg(not(unix))]
impl InterruptHandler {
    pub(crate) fn install() -> std::io::Result<Self> {
        Ok(Self)
    }
    pub(crate) fn requested(&self) -> bool {
        false
    }
}
