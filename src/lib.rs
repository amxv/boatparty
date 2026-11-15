pub mod account;
pub mod cli;
pub mod commands;
pub mod error;
pub mod output;
pub mod paths;
pub mod pool;
pub mod project;
pub mod provider;
pub mod runner;
pub mod runs;
pub mod signals;
pub mod state;
pub mod stream;
pub mod sync;
pub mod system;
pub mod transport;

use std::process::ExitCode;

use clap::Parser;
use cli::{Cli, Command};
use error::AppError;
use output::Output;
use paths::Paths;

/// What every command receives.
pub struct Context {
    pub paths: Paths,
    pub out: Output,
}

/// Parse the command line, run the command, and turn the outcome into the
/// process exit status.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(error::EXIT_USAGE));
        }
    };
    let out = Output::new(cli.json);
    match dispatch(cli, out) {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            out.error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

fn dispatch(cli: Cli, out: Output) -> Result<u8, AppError> {
    let ctx = Context {
        paths: Paths::from_env()?,
        out,
    };
    match cli.command {
        Command::Login(args) => commands::login::run(&ctx, &args),
        Command::Doctor => commands::doctor::run(&ctx),
        Command::Limits => commands::limits::run(&ctx),
        Command::Init(args) => commands::init::run(&ctx, &args),
        Command::Launch => commands::launch::run(&ctx),
        Command::Cheers(args) => commands::cheers::run(&ctx, &args),
        Command::Board(args) => commands::board::run(&ctx, &args),
        Command::Load => commands::load::run(&ctx),
        Command::Fetch(args) => commands::fetch::run(&ctx, &args),
        Command::Status => commands::status::run(&ctx),
        Command::Dock(args) => commands::dock::run(&ctx, &args),
        Command::Rm(args) => commands::rm::run(&ctx, &args),
        Command::Log(args) => commands::log::run(&ctx, &args),
        Command::IdleHelper(args) => {
            pool::lease::idle_helper(&ctx.paths, &args.project, &args.member)?;
            Ok(0)
        }
    }
}
