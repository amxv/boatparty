use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "boatparty",
    version,
    about = env!("CARGO_PKG_DESCRIPTION"),
    after_help = "Exit status: 0 on success, 2 for usage errors, 125 when boatparty itself failed \
                  (configuration, credentials, provider, connection, limits)."
)]
pub struct Cli {
    /// Print exactly one JSON document on stdout instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Store the boat API key, read from standard input.
    Login(LoginArgs),
    /// Check that this machine and the stored API key are ready.
    Doctor,
    /// Show the account's start limits, running sandboxes and included VM time.
    Limits,
}

#[derive(Debug, Args)]
#[command(
    after_help = "The key is read from standard input only, never from an argument, for example:\n  \
                  boat api-key create boatparty --ttl 90d --actions <...> --json | jq -r .secret | boatparty login --key-stdin\n\
                  Run `boatparty doctor` to print the full command."
)]
pub struct LoginArgs {
    /// Read the API key from standard input (required).
    #[arg(long, required = true)]
    pub key_stdin: bool,

    /// Store the key without asking the provider whether it is valid.
    #[arg(long)]
    pub no_verify: bool,
}
