use std::process::ExitCode;

use skilled::{AppEnvironment, run};

fn main() -> ExitCode {
    match AppEnvironment::for_process().and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("skilled: {error}");
            ExitCode::FAILURE
        }
    }
}
