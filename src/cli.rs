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
    /// Start this workspace's sandbox: create or resume it, then connect.
    Launch,
    /// Sync the working tree to the sandbox's checkout (no command runs).
    #[command(
        after_help = "Makes /home/user/<project> on the sandbox identical to this working tree: \
                      tracked and untracked files minus ignored ones, the same HEAD and branch, \
                      every local branch and tag, and the default remote branch. Only files \
                      whose content changed are rewritten there. Ignored files never leave \
                      this machine, and the local index and working tree are not modified."
    )]
    Load,
    /// Open a shell in the sandbox's checkout, or run one command there.
    Board(BoardArgs),
    /// Stop this workspace's sandbox; its disk is kept.
    Dock(DockArgs),
    /// Write boatparty.toml for this Git repository and register the project.
    Init(InitArgs),
    /// Show this project's sandboxes.
    Status,
    /// Delete a sandbox at the provider and forget it.
    Rm(RmArgs),
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

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Project name (lowercase letters, digits and hyphens). Defaults to the
    /// directory name.
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  boatparty board                 interactive shell in /home/user/<project>\n  \
                  boatparty board -- uname -a     run one command; exits with its status\n\
                  Each word after -- is quoted, so `boatparty board -- sh -c 'a | b'` runs exactly that."
)]
pub struct BoardArgs {
    /// The command to run, after `--`. Without one, an interactive shell opens.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DockArgs {
    /// The sandbox to stop (name or id); defaults to this workspace's.
    pub member: Option<String>,

    /// Also wait until the sandbox has stopped.
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    /// The sandbox to delete (name or id); defaults to this workspace's.
    pub member: Option<String>,
}
