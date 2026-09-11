use std::io::stdout;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    Action, AppEnvironment, Result, SkilledApp, UpdateOutcome,
    input::action_for_app_key,
    terminal::{CrosstermControl, InterruptHandler, TerminalSession, install_panic_restore_hook},
    tui,
};

pub fn run(environment: AppEnvironment) -> Result<()> {
    let interrupt = InterruptHandler::install()?;
    install_panic_restore_hook();
    let session = TerminalSession::start(CrosstermControl)?;
    session.run(|| {
        // Own the app inside the restoration boundary: its Drop cancels and
        // joins workers on every return and unwind, before the screen leaves.
        let app = SkilledApp::open(environment)?;
        run_app(app, &interrupt)
    })
}

fn run_app(mut app: SkilledApp, interrupt: &InterruptHandler) -> Result<()> {
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        if interrupt.requested() {
            let update = app.update(Action::Quit);
            app.perform_effects(update.effects())?;
            break;
        }
        app.drain_vendored_check();
        app.drain_vendored_apply();
        let effects = app.drain_update_check();
        app.perform_effects(&effects)?;
        // The frame measures what the reducer cannot see, so its report is
        // taken back before the next key is read: `update` stays free of
        // geometry, and the region the user is looking at is the one the next
        // keystroke is clamped against.
        let mut feedback = tui::RenderFeedback::default();
        terminal.draw(|frame| feedback = tui::render(frame, &app))?;
        // A frame that did not draw the region measured nothing, and nothing is
        // not zero: the offset it was last scrolled to survives the frames
        // where the region is off screen. That the frame measured nothing is
        // itself reported, so a dialog the terminal was too small to draw
        // cannot be confirmed on the strength of an earlier frame's extent.
        for list in crate::app::ListWindow::ALL {
            app.note_list_window_start(list, feedback.list_window_start(list));
        }
        app.note_detail_max_scroll(feedback.detail_max_scroll());
        app.note_update_preview_fully_seen(feedback.update_preview_fully_seen());
        // Bound idle waits too: external SIGINT is a flag, not a key event.
        let event = event::poll(Duration::from_millis(100))?
            .then(event::read)
            .transpose()?;
        if interrupt.requested() {
            continue;
        }
        let Some(Event::Key(key)) = event else {
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

    Ok(())
}
