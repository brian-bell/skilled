pub mod agents;
pub mod app;
mod components;
pub mod error;
pub mod input;
pub mod inventory;
pub mod operations;
pub mod paths;
pub mod resolution;
pub mod runner;
pub mod source;
mod store;
pub mod terminal;
mod theme;
pub mod tui;
pub mod validation;
mod viewport;

pub use agents::{AgentDetection, AgentKind};
pub use app::{
    Action, DoctorPane, Effect, InventoryPane, SetupStep, SkilledApp, SourcesPane, UpdateOutcome,
    UpdateResult, View,
};
pub use error::{Error, Result};
pub use paths::AppEnvironment;
pub use runner::run;
