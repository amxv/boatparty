use clap::{ArgGroup, Args, Parser, Subcommand};

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
                      tracked and untracked files minus ignored ones, the same HEAD and branch. \
                      With the default `[sync] history = \"head\"` in boatparty.toml only HEAD's \
                      commit travels (the sandbox's `git log` shows one commit); with \"full\" \
                      every local branch and tag and the default remote branch follow with their \
                      history. Only files whose content changed are rewritten there. Ignored \
                      files never leave this machine, and the local index and working tree are \
                      not modified. Large packs go through a signed upload URL when the provider \
                      offers one."
    )]
    Load,
    /// Sync, run one command in the sandbox's checkout, stream its output, and
    /// apply its file changes back; exits with the command's status.
    Cheers(CheersArgs),
    /// Open a shell in the sandbox's checkout, or run one command there
    /// without syncing.
    Board(BoardArgs),
    /// Stop this workspace's sandbox; its disk is kept.
    Dock(DockArgs),
    /// Copy a file or directory from the sandbox to this machine (no sync).
    Fetch(FetchArgs),
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
    /// List recent runs (`cheers`), oldest first.
    Log(LogArgs),
    /// Internal: set a member's idle deadline after its last lease ended.
    #[command(name = "__idle", hide = true)]
    IdleHelper(IdleHelperArgs),
}

#[derive(Debug, Args)]
#[command(
    group(ArgGroup::new("what").required(true).args(["sh", "command"])),
    after_help = "Examples:\n  \
                  boatparty cheers -- cargo test --locked     each word after -- is quoted\n  \
                  boatparty cheers --sh 'cargo test 2>&1 | tail -n 20'   one shell line\n\n\
                  The working tree is synced first (as `boatparty load` does). stdout is passed through \
                  byte for byte. Files the command changes in the checkout are applied to this working \
                  tree afterwards; if the same lines changed here meanwhile, nothing is changed and the \
                  patch is kept under the Git directory.\n\n\
                  Exit status: the command's own; 125 when boatparty itself failed; 130 or 143 after \
                  SIGINT or SIGTERM, which also stop the remote command. With --json, stdout carries \
                  newline-delimited frames: stdout, stderr and event frames, then one result frame."
)]
pub struct CheersArgs {
    /// Run this shell line as given instead of a quoted argument list.
    #[arg(long, value_name = "LINE")]
    pub sh: Option<String>,

    /// The command to run, after `--`. Every word is quoted.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// How many runs to show.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct IdleHelperArgs {
    pub project: String,
    pub member: String,
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
#[command(after_help = "Examples:\n  \
                  boatparty fetch target/release/app            into the current directory\n  \
                  boatparty fetch target/release/app /tmp/app-linux   under a new name\n  \
                  boatparty fetch /tmp/report out/              an absolute path, into out/\n\n\
                  A relative PATH is resolved in the sandbox's checkout (/home/user/<project>). \
                  Nothing is synced first. When DEST is an existing directory the copy keeps its \
                  name inside it; otherwise DEST is the copy's new name. An existing file is \
                  replaced; an existing directory is never overwritten. Symbolic links are followed.")]
pub struct FetchArgs {
    /// The file or directory on the sandbox: relative to the checkout, or absolute.
    pub path: String,

    /// Where to put it on this machine. Defaults to the current directory.
    pub dest: Option<std::path::PathBuf>,
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
