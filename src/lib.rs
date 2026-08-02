pub mod agents;
pub mod app;
pub mod error;
pub mod input;
pub mod paths;
pub mod runner;
mod store;
pub mod terminal;
pub mod tui;

pub use agents::{AgentDetection, AgentKind};
pub use app::{Action, SetupStep, SkilledApp, UpdateOutcome, View};
pub use error::{Error, Result};
pub use paths::AppEnvironment;
pub use runner::run;
