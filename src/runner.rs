use std::io::stdout;

use crossterm::event::{self, Event};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    AppEnvironment, Result, SkilledApp, UpdateOutcome,
    input::action_for_key,
    terminal::{CrosstermControl, TerminalSession, install_panic_restore_hook},
    tui,
};

pub fn run(environment: AppEnvironment) -> Result<()> {
    let mut app = SkilledApp::open(environment)?;
    install_panic_restore_hook();
    let session = TerminalSession::start(CrosstermControl)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| tui::render(frame, &app))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let Some(action) = action_for_key(app.view(), key) else {
            continue;
        };
        if app.update(action)? == UpdateOutcome::Quit {
            break;
        }
    }

    drop(terminal);
    session.finish()?;
    Ok(())
}
