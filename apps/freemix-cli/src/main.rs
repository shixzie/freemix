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

use std::process::ExitCode;

fn main() -> ExitCode {
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
