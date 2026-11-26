pub mod account;
pub mod base;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod output;
pub mod paths;
pub mod pool;
pub mod project;
pub mod provider;
pub mod runner;
pub mod runs;
pub mod signals;
pub mod starts;
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
            // clap quotes unexpected arguments. Never echo a misplaced secret.
            if error.use_stderr()
                && std::env::args_os()
                    .skip(1)
                    .find(|a| a != "--json")
                    .is_some_and(|a| a == "secret")
            {
                out_secret_usage();
            } else {
                let _ = error.print();
            }
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(error::EXIT_USAGE));
        }
    };
    let out = Output::new(cli.json);
    // A streaming command's failure is its result frame, even when it fails
    // before streaming anything.
    let streaming = matches!(
        cli.command,
        Command::Run(_)
            | Command::Gate(_)
            | Command::Serve(_)
            | Command::Pipe(_)
            | Command::Shell(_)
    );
    match dispatch(cli, out) {
        Ok(status) => ExitCode::from(status),
        Err(error) if streaming => {
            stream::Stream::new(out.is_json()).error(&error);
            ExitCode::from(error.exit_code())
        }
        Err(error) => {
            out.error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

fn out_secret_usage() {
    let json = std::env::args_os().any(|a| a == "--json");
    Output::new(json).error(&AppError::new(
        "usage",
        "use secret set KEY with the value on standard input, secret rm KEY, or secret list",
    ));
}

fn dispatch(cli: Cli, out: Output) -> Result<u8, AppError> {
    let ctx = Context {
        paths: Paths::from_env()?,
        out,
    };
    match cli.command {
        Command::Login(args) => commands::login::run(&ctx, &args),
        Command::Doctor => commands::doctor::run(&ctx),
        Command::Limits(args) => commands::limits::run(&ctx, &args),
        Command::Init(args) => commands::init::run(&ctx, &args),
        Command::Start => commands::start::run(&ctx),
        Command::Gate(args) => commands::gate::run(&ctx, &args),
        Command::RefreshBase { project, built_at } => {
            commands::base::refresh::run(&ctx, &project, built_at)
        }
        Command::Run(args) => commands::run::run(&ctx, &args),
        Command::Serve(args) => commands::serve::run(&ctx, &args),
        Command::Shell(args) => commands::shell::run(&ctx, &args),
        Command::Url { remote_port } => commands::url::run(&ctx, remote_port),
        Command::Sync => commands::sync::run(&ctx),
        Command::Pipe(args) => commands::pipe::run(&ctx, &args),
        Command::Fetch(args) => commands::fetch::run(&ctx, &args),
        Command::Forward(args) => commands::forward::run(&ctx, &args),
        Command::Desktop { open } => commands::desktop::run(&ctx, open),
        Command::Expose(args) => commands::expose::run(&ctx, &args),
        Command::Secret { action, create } => commands::env::secret(&ctx, &action, create),
        Command::Env { action } => commands::env::run(&ctx, &action),
        Command::Status => commands::status::run(&ctx),
        Command::Pool(args) => commands::pool::run(&ctx, &args),
        Command::Config(args) => commands::config::run(&ctx, &args),
        Command::Preset(args) => commands::preset::run(&ctx, &args),
        Command::Release => commands::pin::release(&ctx),
        Command::Assign(args) => commands::pin::assign(&ctx, &args),
        Command::Stop(args) => commands::stop::run(&ctx, &args),
        Command::Rm(args) => commands::rm::run(&ctx, &args),
        Command::Base(args) => commands::base::run(&ctx, &args),
        Command::Log(args) => commands::log::run(&ctx, &args),
        Command::Skill(args) => commands::skill::run(&ctx, &args),
        Command::IdleHelper(args) => {
            pool::lease::idle_helper(&ctx.paths, &args.project, &args.member)?;
            Ok(0)
        }
    }
}
