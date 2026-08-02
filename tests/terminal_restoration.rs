use std::{
    io,
    sync::{Arc, Mutex},
};

use skilled::terminal::{TerminalControl, TerminalSession};

#[derive(Clone)]
struct RecordingTerminal {
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl TerminalControl for RecordingTerminal {
    fn enter(&mut self) -> io::Result<()> {
        self.events.lock().expect("event log").push("enter");
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        self.events.lock().expect("event log").push("restore");
        Ok(())
    }
}

struct FailingTerminal {
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl TerminalControl for FailingTerminal {
    fn enter(&mut self) -> io::Result<()> {
        self.events.lock().expect("event log").push("enter");
        Err(io::Error::other("simulated terminal startup failure"))
    }

    fn restore(&mut self) -> io::Result<()> {
        self.events.lock().expect("event log").push("restore");
        Ok(())
    }
}

#[test]
fn dropping_an_active_session_restores_the_terminal() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let control = RecordingTerminal {
        events: Arc::clone(&events),
    };

    let session = TerminalSession::start(control).expect("start terminal session");
    drop(session);

    assert_eq!(*events.lock().expect("event log"), ["enter", "restore"]);
}

#[test]
fn finishing_a_session_restores_the_terminal_exactly_once() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let control = RecordingTerminal {
        events: Arc::clone(&events),
    };

    TerminalSession::start(control)
        .expect("start terminal session")
        .finish()
        .expect("finish terminal session");

    assert_eq!(*events.lock().expect("event log"), ["enter", "restore"]);
}

#[test]
fn unwinding_a_panic_restores_the_terminal() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let control = RecordingTerminal {
        events: Arc::clone(&events),
    };

    let panic_result = std::panic::catch_unwind(|| {
        let _session = TerminalSession::start(control).expect("start terminal session");
        panic!("simulated render panic");
    });

    assert!(panic_result.is_err());
    assert_eq!(*events.lock().expect("event log"), ["enter", "restore"]);
}

#[test]
fn a_partial_startup_failure_attempts_restoration() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let result = TerminalSession::start(FailingTerminal {
        events: Arc::clone(&events),
    });

    assert!(result.is_err());
    assert_eq!(*events.lock().expect("event log"), ["enter", "restore"]);
}
