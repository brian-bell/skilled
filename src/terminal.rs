use std::io::{self, stdout};

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

pub fn install_panic_restore_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let mut terminal = CrosstermControl;
        let _ = terminal.restore();
        previous_hook(panic_info);
    }));
}
