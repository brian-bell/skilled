use std::io::stdout;

use crossterm::event::{self, Event};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    AppEnvironment, Result, SkilledApp, UpdateOutcome,
    input::action_for_app_key,
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
        // The frame measures what the reducer cannot see, so its report is
        // taken back before the next key is read: `update` stays free of
        // geometry, and the region the user is looking at is the one the next
        // keystroke is clamped against.
        let mut feedback = tui::RenderFeedback::default();
        terminal.draw(|frame| feedback = tui::render(frame, &app))?;
        // A frame that did not draw the region measured nothing, and nothing
        // is not zero: the offset it was last scrolled to survives the frames
        // where the region is off screen.
        if let Some(max_scroll) = feedback.inventory_detail_max_scroll() {
            app.note_inventory_detail_max_scroll(max_scroll);
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        let Some(action) = action_for_app_key(&app, key) else {
            continue;
        };
        let update = app.update(action);
        app.perform_effects(update.effects())?;
        if update.outcome() == UpdateOutcome::Quit {
            break;
        }
    }

    drop(terminal);
    session.finish()?;
    Ok(())
}
