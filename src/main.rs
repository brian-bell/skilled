use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
};

use skilled::{AppEnvironment, SessionIdentity, cli, run};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // Version discovery is deliberately earlier than every process-dependent
    // input. Package managers and scripts can identify this executable without
    // a terminal, a home directory, application data, or an agent scan.
    if arguments.as_slice() == ["--version"] {
        println!("skilled {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let environment = match AppEnvironment::for_process() {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("skilled: {error}");
            return ExitCode::FAILURE;
        }
    };
    // No arguments is the interactive application, which is what Skilled has
    // always been. Anything else is a command, and a command reports through
    // an exit status a script can act on. Only the interactive application
    // gathers the session identity its title bar shows; a command never runs
    // the hostname lookup whose result it would not display.
    if arguments.is_empty() {
        return match run(environment.with_identity(SessionIdentity::for_process())) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("skilled: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let mut input: Box<dyn BufRead> = Box::new(io::stdin().lock());
    let mut output: Box<dyn Write> = Box::new(io::stdout().lock());
    let code = cli::run(&arguments, environment, &mut input, &mut output);
    let _ = output.flush();
    ExitCode::from(code.code())
}
