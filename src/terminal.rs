use std::{
    cell::Cell,
    io::{self, stdout},
};

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
/// event loop. The prior process hook still receives every panic except an
/// update-worker panic that its effect boundary catches and reports in-app.
pub fn install_panic_restore_hook() {
    let terminal_thread = std::thread::current().id();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let owns_terminal = std::thread::current().id() == terminal_thread;
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
    use super::should_chain_previous_hook;

    #[test]
    fn only_caught_non_terminal_worker_panics_skip_the_previous_printer() {
        assert!(!should_chain_previous_hook(false, true));
        assert!(should_chain_previous_hook(false, false));
        assert!(should_chain_previous_hook(true, true));
        assert!(should_chain_previous_hook(true, false));
    }
}
