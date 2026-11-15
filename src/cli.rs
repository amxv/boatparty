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
    /// Show every project's sandboxes: state, workspace, leases, last use.
    Pool(PoolArgs),
    /// Show or check this project's configuration (boatparty.toml and its presets).
    Config(ConfigArgs),
    /// List, print or copy the presets a boatparty.toml can name.
    Preset(PresetArgs),
    /// Let go of this workspace's sandbox so another workspace may use it.
    #[command(
        after_help = "The sandbox keeps running or stays stopped; only the record that ties it to \
                      this workspace is dropped. Refused while a command uses it. The next command \
                      in this workspace takes a free sandbox, or the same one if it is still free."
    )]
    Release,
    /// Pin a free sandbox of this project to this workspace.
    Assign(AssignArgs),
    /// Delete a sandbox at the provider and forget it.
    Rm(RmArgs),
    /// Store the boat API key, read from standard input.
    Login(LoginArgs),
    /// Check that this machine and the stored API key are ready.
    Doctor,
    /// Show the account's and every project's limits and usage, or change a
    /// limit with `limits set`.
    Limits(LimitsArgs),
    /// List recent runs (`cheers`), oldest first.
    Log(LogArgs),
    /// Internal: set a member's idle deadline after its last lease ended.
    #[command(name = "__idle", hide = true)]
    IdleHelper(IdleHelperArgs),
}

#[derive(Debug, Args)]
#[command(
    group(ArgGroup::new("what").required(true).multiple(true).args(["sh", "command", "words"])),
    after_help = "Examples:\n  \
                  boatparty cheers -- cargo test --locked     each word after -- is quoted\n  \
                  boatparty cheers --sh 'cargo test 2>&1 | tail -n 20'   one shell line\n  \
                  boatparty cheers test -q                    [commands] test, with -q appended\n  \
                  boatparty cheers check:rust                 or the script runner: `bun run check:rust`\n\n\
                  A NAME runs the line `[commands] NAME` from boatparty.toml (after presets apply) \
                  with ARGS quoted and appended. Without such an entry it runs `[run] script NAME \
                  ARGS` when the project sets a script runner (the node, pnpm and bun presets do); \
                  otherwise it fails and lists the known names. Options such as --json go before \
                  NAME; everything after NAME is passed on, `--` included \
                  (`boatparty cheers test -- --nocapture` appends `-- --nocapture`).\n\n\
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
    #[arg(long, value_name = "LINE", conflicts_with = "command")]
    pub sh: Option<String>,

    /// A command named in boatparty.toml, then its arguments.
    #[arg(
        value_name = "NAME [ARGS]",
        allow_hyphen_values = true,
        conflicts_with = "sh"
    )]
    pub words: Vec<String>,

    /// The command to run, after `--`. Every word is quoted.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Shows the provider's start windows, running sandboxes and included VM time, \
                  then for each project: members, running sandboxes, starts in the last hour and \
                  day against its caps, and this month's machine cost. Limits live in \
                  account.toml; `boatparty limits set` changes them without touching its comments."
)]
pub struct LimitsArgs {
    #[command(subcommand)]
    pub action: Option<LimitsAction>,
}

#[derive(Debug, Subcommand)]
pub enum LimitsAction {
    /// Change limits in account.toml, keeping its comments and layout.
    #[command(subcommand)]
    Set(LimitsTarget),
}

#[derive(Debug, Subcommand)]
#[command(after_help = "Examples:\n  \
                  boatparty limits set account max_running=4 reserve_starts_per_day=20\n  \
                  boatparty limits set project app starts_per_hour=10 max_machine=default\n  \
                  boatparty limits set project app max_running=     (an empty value removes the key)")]
pub enum LimitsTarget {
    /// Account-wide limits ([account]) and defaults for every project ([defaults]):
    /// max_running, reserve_starts_per_hour, reserve_starts_per_day, machine,
    /// max_sandboxes, steal_after_minutes, lease_minutes (at most 5),
    /// idle_minutes (at most 15), heartbeat_minutes, acquire_wait_seconds.
    Account {
        /// KEY=VALUE pairs.
        #[arg(required = true, value_name = "KEY=VALUE")]
        assignments: Vec<String>,
    },
    /// One project's limits ([projects.NAME]): max_sandboxes, max_running,
    /// max_machine, starts_per_hour, starts_per_day, idle_minutes (at most 15),
    /// lease_minutes (at most 5).
    Project {
        /// The project's name.
        name: String,
        /// KEY=VALUE pairs.
        #[arg(required = true, value_name = "KEY=VALUE")]
        assignments: Vec<String>,
    },
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
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n  \
                  boatparty pool                 every project's sandboxes\n  \
                  boatparty pool --refresh       first match the records with the provider\n  \
                  boatparty pool prune           delete stopped sandboxes no workspace needs\n\n\
                  A project owns at most `max_sandboxes` sandboxes (account.toml: [defaults], or \
                  [projects.<name>] for one project). A workspace keeps using the sandbox it used \
                  last; when it has none it takes a free one, then a new one below the limit, then \
                  an idle one from another workspace (one whose directory is gone first, then one \
                  unused for `steal_after_minutes`), and otherwise waits up to \
                  `acquire_wait_seconds`.\n\n\
                  --refresh adopts sandboxes that carry the project's label but have no record, \
                  drops records whose sandbox is gone, deletes sandboxes whose stored disk the \
                  provider expired, and frees sandboxes whose workspace directory no longer \
                  exists. A missing directory never deletes anything."
)]
pub struct PoolArgs {
    /// Reconcile the records with the provider's labelled sandboxes first.
    #[arg(long)]
    pub refresh: bool,

    /// Only this project.
    #[arg(long, value_name = "NAME")]
    pub project: Option<String>,

    #[command(subcommand)]
    pub action: Option<PoolAction>,
}

#[derive(Debug, Subcommand)]
pub enum PoolAction {
    /// Delete stopped sandboxes that no workspace needs.
    Prune(PruneArgs),
}

#[derive(Debug, Args)]
#[command(
    after_help = "Deletes, at the provider, every stopped pool sandbox that has no workspace or \
                  whose workspace directory no longer exists, and is not in use. It lists them \
                  and asks first (reading the answer from standard input) unless --yes is given. \
                  A sandbox whose workspace still exists is never deleted."
)]
pub struct PruneArgs {
    /// Delete without asking.
    #[arg(long)]
    pub yes: bool,

    /// Only this project.
    #[arg(long, value_name = "NAME")]
    pub project: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "MEMBER is a sandbox name or id from `boatparty pool`. It must be free: not in \
                  use and not tied to another workspace that still exists. This workspace's \
                  previous sandbox, if any, becomes free."
)]
pub struct AssignArgs {
    /// The sandbox to pin (name or id).
    pub member: String,
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

#[derive(Debug, Args)]
#[command(after_help = crate::config::HELP)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print boatparty.toml as parsed, or with --resolved the effective
    /// configuration after presets and defaults.
    Show(ConfigShowArgs),
    /// Check boatparty.toml, the presets it names and every preset file in
    /// the user and project presets directories.
    Validate,
}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// The effective configuration: defaults, then the presets in order,
    /// then boatparty.toml.
    #[arg(long)]
    pub resolved: bool,

    /// Note where each value came from (implies --resolved).
    #[arg(long)]
    pub origin: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Presets live in three places, from least to most specific: built into \
                  boatparty, ~/.config/boatparty/presets/<name>.toml (the user's) and \
                  .boatparty/presets/<name>.toml in the project. A file in a more specific place \
                  replaces the one of the same name below it entirely. `preset eject` copies a \
                  built-in there so it can be edited as plain text."
)]
pub struct PresetArgs {
    #[command(subcommand)]
    pub action: PresetAction,
}

#[derive(Debug, Subcommand)]
pub enum PresetAction {
    /// List every preset, where it comes from and whether this project uses it.
    List,
    /// Print a preset's text (the one a project would get).
    Show {
        /// The preset's name.
        name: String,
    },
    /// Copy a built-in preset into the project's .boatparty/presets/ (or,
    /// with --user, the user's presets directory) for editing.
    Eject(PresetEjectArgs),
}

#[derive(Debug, Args)]
pub struct PresetEjectArgs {
    /// The built-in preset to copy.
    pub name: String,

    /// Copy it to the user's presets directory instead, for every project.
    #[arg(long)]
    pub user: bool,

    /// Replace an existing file.
    #[arg(long)]
    pub force: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheers(args: &[&str]) -> Result<CheersArgs, clap::Error> {
        let mut argv = vec!["boatparty", "cheers"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Command::Cheers(args) => args,
            other => panic!("parsed {other:?}"),
        })
    }

    #[test]
    fn cheers_takes_a_name_with_arguments_a_quoted_argv_or_a_shell_line() {
        let named = cheers(&["test", "-q", "--nocapture"]).unwrap();
        assert_eq!(named.words, ["test", "-q", "--nocapture"]);
        assert!(named.command.is_empty() && named.sh.is_none());
        let argv = cheers(&["--", "cargo", "test"]).unwrap();
        assert!(argv.words.is_empty());
        assert_eq!(argv.command, ["cargo", "test"]);
        let line = cheers(&["--sh", "a | b"]).unwrap();
        assert_eq!(line.sh.as_deref(), Some("a | b"));
        // After NAME everything is passed on, `--` included.
        let both = cheers(&["test", "--", "--nocapture"]).unwrap();
        assert_eq!(both.words, ["test", "--", "--nocapture"]);
        assert!(both.command.is_empty());
        assert!(cheers(&[]).is_err());
        assert!(cheers(&["--sh", "x", "--", "y"]).is_err());
        assert!(cheers(&["--sh", "x", "test"]).is_err());
    }
}
