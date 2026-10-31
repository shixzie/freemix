mod app;
mod args;
mod auto_key;
mod grading;
mod presets;
mod project_bundle;
mod remote;
mod scene_animation;
mod scene_render;
mod shortcut_sets;
mod title_animation;
mod title_row;

use std::process::ExitCode;

/// The stack the command runs on. A Windows main thread has 1 MiB, which
/// the project and journal work overflows; this is what a Linux main
/// thread gets.
const COMMAND_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    let command = match std::thread::Builder::new()
        .name("freemix-cli".into())
        .stack_size(COMMAND_STACK_BYTES)
        .spawn(run)
    {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: cannot start the command thread: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A panic has already been reported by the panic hook; its exit status
    // is the one a panicking main thread gives.
    command.join().unwrap_or(ExitCode::from(101))
}

fn run() -> ExitCode {
    let command = match args::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("run `freemix-cli help` for usage");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = app::run(command) {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
