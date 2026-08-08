use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
};

use skilled::{AppEnvironment, cli, run};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let environment = match AppEnvironment::for_process() {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("skilled: {error}");
            return ExitCode::FAILURE;
        }
    };
    // No arguments is the interactive application, which is what Skilled has
    // always been. Anything else is a command, and a command reports through
    // an exit status a script can act on.
    if arguments.is_empty() {
        return match run(environment) {
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
