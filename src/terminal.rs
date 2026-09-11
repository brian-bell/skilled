use std::{
    cell::{Cell, RefCell},
    io::{self, Write, stdout},
};

mod interrupt;
pub(crate) use interrupt::InterruptHandler;

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

pub trait TerminalControl {
    fn enter(&mut self) -> io::Result<()>;
    fn restore(&mut self) -> io::Result<()>;
}

pub struct TerminalSession<C: TerminalControl> {
    control: Option<C>,
}

impl<C: TerminalControl> TerminalSession<C> {
    pub fn start(mut control: C) -> io::Result<Self> {
        if let Err(error) = control.enter() {
            let _ = control.restore();
            return Err(error);
        }
        Ok(Self {
            control: Some(control),
        })
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.control
            .take()
            .expect("active terminal session")
            .restore()
    }

    /// Run terminal-owned work, dropping its workers before restoring the
    /// screen on success, error, or unwind. Install the panic restore hook
    /// before starting the session. The closure must own the work it retires.
    /// Panics retain their payload and are reported after terminal restoration.
    pub fn run<T, E: From<io::Error>>(
        self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        struct DeferredPanic(bool);
        impl Drop for DeferredPanic {
            fn drop(&mut self) {
                DEFER_TERMINAL_PANIC.with(|deferred| deferred.set(self.0));
            }
        }
        let deferred = DeferredPanic(DEFER_TERMINAL_PANIC.with(|flag| flag.replace(true)));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        drop(deferred);
        let restored = self.finish().map_err(E::from);
        match outcome {
            Ok(result) => result.and_then(|value| restored.map(|()| value)),
            Err(payload) => {
                DEFERRED_PANIC_MESSAGE.with(|message| {
                    if let Some(message) = message.borrow_mut().take() {
                        for line in message.lines() {
                            let _ = writeln!(
                                io::stderr(),
                                "{}",
                                crate::components::terminal_safe(line)
                            );
                        }
                    }
                });
                std::panic::resume_unwind(payload)
            }
        }
    }
}

impl<C: TerminalControl> Drop for TerminalSession<C> {
    fn drop(&mut self) {
        if let Some(control) = &mut self.control {
            let _ = control.restore();
        }
    }
}

pub struct CrosstermControl;

thread_local! {
    static CAUGHT_WORKER_PANIC: Cell<bool> = const { Cell::new(false) };
    static DEFER_TERMINAL_PANIC: Cell<bool> = const { Cell::new(false) };
    static DEFERRED_PANIC_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Catch a worker panic without letting the process-global default hook print
/// into the live alternate screen. Other background threads still chain to the
/// hook that was installed before Skilled took terminal ownership.
pub(crate) fn catch_update_worker_panic<F, R>(operation: F) -> std::thread::Result<R>
where
    F: FnOnce() -> R,
{
    CAUGHT_WORKER_PANIC.with(|caught| {
        caught.set(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        caught.set(false);
        result
    })
}

impl TerminalControl for CrosstermControl {
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, Hide)?;
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        let screen_result = execute!(stdout(), Show, LeaveAlternateScreen);
        let raw_result = disable_raw_mode();
        screen_result.and(raw_result)
    }
}

/// Restore only for a panic on the thread that owns the terminal.
///
/// Panic hooks are process-global and run before unwinding, so a worker panic
/// must not tear down raw mode and the alternate screen under the still-live
/// event loop. Inside [`TerminalSession::run`], terminal-thread diagnostics are
/// deferred until worker teardown and restoration finish, then the original
/// panic payload resumes unwinding. This boundary captures the diagnostic and
/// requested backtrace itself; it does not invoke a prior custom panic hook
/// into the live terminal. Outside that boundary, terminal-thread
/// panics restore immediately and chain to the prior hook. Caught worker
/// panics are reported in-app; other worker panics chain to the prior hook.
pub fn install_panic_restore_hook() {
    let terminal_thread = std::thread::current().id();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let owns_terminal = std::thread::current().id() == terminal_thread;
        // The runner catches this unwind so the application can retire its
        // workers first. Printing or restoring here would hand the terminal
        // back while those workers still own it. Outside that boundary the
        // original immediate-restoration fallback remains in place.
        if owns_terminal && DEFER_TERMINAL_PANIC.with(Cell::get) {
            DEFERRED_PANIC_MESSAGE.with(|message| {
                // PanicHookInfo borrows the original panic and cannot be
                // retained until after unwind. Capture equivalent diagnostics
                // on this stack rather than invoking an arbitrary prior hook
                // while workers and the alternate screen are still active.
                let backtrace = std::backtrace::Backtrace::capture();
                let diagnostic = if backtrace.status() == std::backtrace::BacktraceStatus::Captured
                {
                    format!("{panic_info}\nstack backtrace:\n{backtrace}")
                } else {
                    panic_info.to_string()
                };
                *message.borrow_mut() = Some(diagnostic);
            });
            return;
        }
        if owns_terminal {
            let mut terminal = CrosstermControl;
            let _ = terminal.restore();
        }
        let caught_worker = CAUGHT_WORKER_PANIC.with(Cell::get);
        if should_chain_previous_hook(owns_terminal, caught_worker) {
            previous_hook(panic_info);
        }
    }));
}

fn should_chain_previous_hook(owns_terminal: bool, caught_worker: bool) -> bool {
    owns_terminal || !caught_worker
}

#[cfg(test)]
mod tests {
    use super::{
        catch_update_worker_panic, install_panic_restore_hook, should_chain_previous_hook,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    static PANIC_HOOK_TEST: Mutex<()> = Mutex::new(());
    type PreviousPanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

    #[test]
    fn only_caught_non_terminal_worker_panics_skip_the_previous_printer() {
        assert!(!should_chain_previous_hook(false, true));
        assert!(should_chain_previous_hook(false, false));
        assert!(should_chain_previous_hook(true, true));
        assert!(should_chain_previous_hook(true, false));
    }

    #[test]
    fn a_caught_worker_panic_does_not_invoke_the_previous_hook() {
        let _guard = PANIC_HOOK_TEST
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let printed = Arc::new(AtomicBool::new(false));
        let previous = std::panic::take_hook();
        struct RestoreHook(Option<PreviousPanicHook>);
        impl Drop for RestoreHook {
            fn drop(&mut self) {
                if let Some(previous) = self.0.take() {
                    std::panic::set_hook(previous);
                }
            }
        }
        let _restore = RestoreHook(Some(previous));
        {
            let printed = Arc::clone(&printed);
            std::panic::set_hook(Box::new(move |_| {
                printed.store(true, Ordering::SeqCst);
            }));
        }
        install_panic_restore_hook();

        let caught = std::thread::spawn(|| {
            let _ = catch_update_worker_panic(|| panic!("injected vendored worker panic"));
        });
        caught
            .join()
            .expect("caught worker panic stays on the worker");
        assert!(
            !printed.load(Ordering::SeqCst),
            "a caught worker panic must not print through the previous hook"
        );

        let uncaught = std::thread::spawn(|| panic!("uncaught worker panic"));
        assert!(uncaught.join().is_err());
        assert!(
            printed.load(Ordering::SeqCst),
            "a panic outside the catch boundary must still reach the previous hook"
        );
    }
}
