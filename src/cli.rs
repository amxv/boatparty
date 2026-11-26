use clap::{ArgGroup, Args, Parser, Subcommand};

#[derive(Debug, Subcommand)]
pub enum SecretAction {
    /// Set KEY to the UTF-8 value on standard input, preserving newlines.
    Set { key: String },
    /// Remove KEY from the configured environment.
    Rm { key: String },
    /// List variable names only.
    List,
}

#[derive(Debug, Subcommand)]
pub enum EnvAction {
    /// Show each member's environment, pinned version and injected variable names.
    Status,
    /// Upgrade an old pin. Members on another environment need recreation.
    Apply {
        #[arg(conflicts_with = "all")]
        member: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

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
    /// When included VM hours are exhausted, a start reports billing once per project per UTC day.
    Start,
    /// Sync the working tree to the sandbox's checkout (no command runs).
    #[command(
        after_help = "Makes the sandbox's checkout (/home/user/<project>, or [sandbox] remote_path) identical to this working tree: \
                      tracked and untracked files minus ignored ones, the same HEAD and branch. \
                      With the default `[sync] history = \"head\"` in boatparty.toml only HEAD's \
                      commit travels (the sandbox's `git log` shows one commit); with \"full\" \
                      every local branch and tag and the default remote branch follow with their \
                      history. Only files whose content changed are rewritten there. Ignored \
                      files never leave this machine, and the local index and working tree are \
                      not modified. Large packs go through a signed upload URL when the provider \
                      offers one. Sync waits for active runs and can update a running serve."
    )]
    Sync,
    /// Sync, run one command in the sandbox's checkout, stream its output, and
    /// apply its file changes back; exits with the command's status.
    Run(RunArgs),
    /// Run checks on a committed revision using the project's exclusive gate member.
    Gate(GateArgs),
    #[command(hide = true)]
    RefreshBase { project: String, built_at: u64 },
    /// Run a long-lived process, such as a dev server, in the sandbox's
    /// checkout until cancelled, with optional port forwards.
    Serve(ServeArgs),
    /// Print local forward URLs of running serves in this workspace, one per line.
    Url {
        /// Show only forwards to this sandbox port.
        #[arg(value_name = "REMOTEPORT", value_parser = clap::value_parser!(u16).range(1..))]
        remote_port: Option<u16>,
    },
    /// Open a shell in the sandbox's checkout, or run one command there
    /// without syncing.
    Shell(ShellArgs),
    /// Stop this workspace's sandbox, keeping its disk. Refused while a command holds a lease.
    Stop(StopArgs),
    /// Run one command in the sandbox's checkout with this process's standard
    /// input (no sync); for input that must not be in a command line or a log.
    Pipe(PipeArgs),
    /// Copy a file or directory from the sandbox to this machine (no sync).
    Fetch(FetchArgs),
    /// Forward a local port to a port on the sandbox until cancelled.
    Forward(ForwardArgs),
    /// Expose a sandbox port on a public HTTPS URL (token-gated unless
    /// --public); prints the URL.
    Expose(ExposeArgs),
    /// Start this sandbox's streaming desktop and print a private, signed HTTPS link.
    Desktop {
        /// Open the link in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// Write boatparty.toml for this Git repository and register the project.
    Init(InitArgs),
    /// Manage this project's environment variables. Values come only from stdin.
    Secret {
        /// Create a secrets-only environment named after the project when unset.
        #[arg(long, global = true)]
        create: bool,
        #[command(subcommand)]
        action: SecretAction,
    },
    /// Inspect or update project members' environment pins.
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
    /// Show this project's sandboxes, active serves, forward URLs and environment drift.
    Status,
    /// Show every project's sandboxes: state, workspace, leases, last use and environment drift.
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
    /// Build, inspect or remove this project's base: a saved snapshot that
    /// new sandboxes start from with dependencies already built.
    Base(BaseArgs),
    /// Create a scoped boat API key or store one from standard input.
    Login(LoginArgs),
    /// Check that this machine and the stored API key are ready.
    Doctor,
    /// Show the account's and every project's limits and usage, or change a
    /// limit with `limits set`.
    Limits(LimitsArgs),
    /// List recent runs (`run`, `serve`, `pipe`), oldest first.
    Log(LogArgs),
    /// Write boatparty's skill for coding agents to the state directory and
    /// print its path (installs nothing).
    Skill(SkillArgs),
    /// Internal: set a member's idle deadline after its last lease ended.
    #[command(name = "__idle", hide = true)]
    IdleHelper(IdleHelperArgs),
}

#[derive(Debug, Args)]
#[command(
    group(ArgGroup::new("what").required(true).multiple(true).args(["sh", "command", "words"])),
    after_help = "Examples:\n  \
                  boatparty run -- cargo test --locked     each word after -- is quoted\n  \
                  boatparty run --sh 'cargo test 2>&1 | tail -n 20'   one shell line\n  \
                  boatparty run test -q                    [commands] test, with -q appended\n  \
                  boatparty run check:rust                 or the script runner: `bun run check:rust`\n\n\
                  A NAME runs the line `[commands] NAME` from boatparty.toml (after presets apply) \
                  with ARGS quoted and appended. Without such an entry it runs `[run] script NAME \
                  ARGS` when the project sets a script runner (the node, pnpm and bun presets do); \
                  otherwise it fails and lists the known names. Options such as --json go before \
                  NAME; everything after NAME is passed on, `--` included \
                  (`boatparty run test -- --nocapture` appends `-- --nocapture`).\n\n\
                  Runs sharing a workspace take turns through copy-back. The working tree is synced first (as `boatparty sync` does). stdout is passed through \
                  byte for byte. Files the command changes in the checkout are applied to this working \
                  tree afterwards; if the same lines changed here meanwhile, nothing is changed and the \
                  patch is kept under the Git directory.\n\n\
                  Exit status: the command's own; 125 when boatparty itself failed; 130 or 143 after \
                  SIGINT or SIGTERM, which also stop the remote command. With --json, stdout carries \
                  newline-delimited frames: stdout, stderr and event frames, then one result frame."
)]
pub struct RunArgs {
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
    after_help = "Examples:\n  boatparty gate\n  boatparty gate --commit HEAD~1 test\n  boatparty gate --default-branch -- make check\n\nDefaults to HEAD and the [gate] command shell line. A concurrent gate is refused. Edits from checks are saved as a patch under the local Git directory."
)]
pub struct GateArgs {
    /// Git revision to check (default HEAD).
    #[arg(long, conflicts_with = "default_branch")]
    pub commit: Option<String>,
    /// Check the locally known default branch.
    #[arg(long)]
    pub default_branch: bool,
    /// A named project command and its arguments.
    #[arg(value_name = "NAME [ARGS]", conflicts_with = "command")]
    pub words: Vec<String>,
    /// Command and arguments, after `--`.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    group(ArgGroup::new("what").required(true).multiple(true).args(["sh", "command", "words"])),
    after_help = "Examples:\n  \
                  boatparty serve --forward 8000:8000 -- python3 -m http.server 8000\n  \
                  boatparty serve --forward 3000:3000 --ready-port 3000 dev\n  \
                  boatparty serve --reverse 20000:7777 --sh 'API=http://127.0.0.1:20000 npm start'\n\n\
                  Takes its command in the same three forms as `run` (-- ARGV, --sh LINE, or a \
                  NAME from [commands] or the script runner); options go before NAME. The working \
                  tree is synced first, with the same credentials, [run.env] and steps as \
                  `run`, and the server starts once the sandbox's files are fully restored. It \
                  holds the sandbox (a `serve` lease) until it ends.\n\n\
                  --forward [BIND:]LOCAL:REMOTE makes LOCAL on this machine reach REMOTE on the \
                  sandbox; --reverse [BIND:]REMOTE:LOCAL makes REMOTE on the sandbox reach LOCAL \
                  here. Both listen on 127.0.0.1 unless BIND names another address (for example \
                  0.0.0.0, which other machines can reach). Forwards travel on the server's own \
                  SSH connection and end with it; other commands keep using the shared one.\n\n\
                  Use boatparty url [REMOTEPORT] to discover a running forward. A second serve claiming the same sandbox port fails. Send edits with boatparty sync once the server starts. With an automatic local port, readiness defaults to its remote port.\n\n\
                  --ready-port PORT logs once, and sends the event `ready`, when something starts \
                  listening on PORT on the sandbox. --max-minutes stops a forgotten server (default \
                  120; 0 disables the cap).\n\n\
                  Ctrl-C (SIGINT) or SIGTERM stops the server; if this process dies in any other \
                  way, SIGKILL included, the sandbox stops the server within seconds. Files the \
                  server changed in the checkout come back like `run` only when it exits by \
                  itself; a stopped server sends nothing back, and the next sync replaces them.\n\n\
                  Exit status: the server's own; 125 when boatparty itself failed; 130 or 143 \
                  after SIGINT or SIGTERM. With --json, stdout carries the same frames as \
                  `run`."
)]
pub struct ServeArgs {
    /// Run in the sandbox desktop session. Without --ready-port, signal ready on its first visible window.
    #[arg(long)]
    pub desktop: bool,
    /// Forward LOCAL on this machine to REMOTE on the sandbox (repeatable). Use auto:REMOTE for a free port. Exports BOATPARTY_FORWARD_URL (first), BOATPARTY_FORWARD_URLS (space-separated) and BOATPARTY_FORWARD_<REMOTEPORT>_URL before [run.env].
    #[arg(
        long = "forward",
        value_name = "[BIND:]LOCAL:REMOTE",
        value_parser = crate::transport::forward::parse_local
    )]
    pub forward: Vec<crate::transport::forward::Forward>,

    /// Forward REMOTE on the sandbox to LOCAL on this machine (repeatable).
    #[arg(
        long = "reverse",
        value_name = "[BIND:]REMOTE:LOCAL",
        value_parser = crate::transport::forward::parse_reverse
    )]
    pub reverse: Vec<crate::transport::forward::Forward>,

    /// Log once and send the event `ready` when this sandbox port listens.
    #[arg(long, value_name = "PORT", value_parser = crate::transport::forward::parse_port)]
    pub ready_port: Option<u16>,

    /// Stop the server after this many minutes (0: never).
    #[arg(long, value_name = "MINUTES", default_value = "120", value_parser = parse_minutes)]
    pub max_minutes: f64,

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

impl ServeArgs {
    /// The command, in the shape `run` takes it.
    pub fn command_args(&self) -> RunArgs {
        RunArgs {
            sh: self.sh.clone(),
            words: self.words.clone(),
            command: self.command.clone(),
        }
    }
}

/// A number of minutes, zero or more (fractions allowed).
fn parse_minutes(value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(minutes) if minutes.is_finite() && minutes >= 0.0 => Ok(minutes),
        _ => Err(format!("{value:?} is not a number of minutes (0 or more)")),
    }
}

#[derive(Debug, Args)]
#[command(
    group(ArgGroup::new("what").required(true).args(["sh", "command"])),
    after_help = "Examples:\n  \
                  printf '%s\\n' \"$UPLOAD_URL\" | boatparty pipe -- bash scripts/upload.sh\n  \
                  boatparty pipe --sh 'cat > ~/.config/app/token && chmod 600 ~/.config/app/token' < token.txt\n\n\
                  Runs the command in the sandbox's checkout (/home/user/<project>, or [sandbox] remote_path) with this \
                  process's standard input as its standard input, byte for byte. Nothing is synced \
                  first and no setup or ensure step runs; the project's credentials ([run] \
                  git_identity and github_token) and [run.env] apply as for `run`. The input is \
                  never written to runs.jsonl, state.json or boatparty's messages, so use pipe, not \
                  an argument, for a secret. Files the command changes stay on the sandbox.\n\n\
                  If this process dies in any way, SIGKILL included, the sandbox stops the command \
                  within seconds, even while its input is still open. stdout is passed through \
                  byte for byte. Exit status: the command's own; 125 when boatparty itself \
                  failed; 130 or 143 after SIGINT or SIGTERM, which also stop the command. With \
                  --json, stdout carries the frames `run` uses, then one result frame."
)]
pub struct PipeArgs {
    /// Use the project's exclusive gate member.
    #[arg(long)]
    pub gate: bool,
    /// Run this shell line as given instead of a quoted argument list.
    #[arg(long, value_name = "LINE", conflicts_with = "command")]
    pub sh: Option<String>,

    /// The command to run, after `--`. Every word is quoted.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Writes the skill (a SKILL.md that teaches a coding agent to use \
                  boatparty, matching this binary's version) to \
                  <state dir>/skill/boatparty/SKILL.md and prints that path. It installs nothing: \
                  copy the directory into an agent's skills directory only if the user asks for \
                  it. --print writes the skill to stdout instead of a file."
)]
pub struct SkillArgs {
    /// Print the skill's text on stdout instead of writing the file.
    #[arg(long)]
    pub print: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:\n  \
                  boatparty forward 5432               127.0.0.1:5432 here reaches port 5432 on the sandbox\n  \
                  boatparty forward 8000:18000         127.0.0.1:18000 here reaches port 8000 there\n  \
                  boatparty forward 8000 --bind 0.0.0.0   also reachable from other machines\n\n\
                  Holds the forward, and the sandbox (a `session` lease), until Ctrl-C or SIGTERM; \
                  it ends too if this process dies. The forward listens on 127.0.0.1 unless --bind \
                  names another address, and uses an SSH connection of its own. A ready line goes \
                  to stderr once it listens. When the connection drops, it is reopened (bringing \
                  the sandbox up again if needed) for up to 10 minutes. Exit status: 130 or 143 \
                  after SIGINT or SIGTERM; 125 when the connection failed or could not be reopened.")]
pub struct ForwardArgs {
    /// Local listening port, or auto to choose a free port.
    #[arg(long, value_parser = crate::transport::forward::parse_local_port)]
    pub local: Option<u16>,
    /// The sandbox port, then optionally the local port (the same by default).
    #[arg(value_name = "REMOTE[:LOCAL]", value_parser = crate::transport::forward::parse_pair)]
    pub ports: crate::transport::forward::PortPair,

    /// The local address to listen on instead of 127.0.0.1.
    #[arg(long, value_name = "ADDR", value_parser = crate::transport::forward::parse_bind)]
    pub bind: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:\n  \
                  boatparty expose 8000                     a URL that only works with its token\n  \
                  boatparty expose 5173 --localhost         for a service bound to 127.0.0.1\n  \
                  boatparty expose 8000 --public --title docs   anyone can open it\n  \
                  boatparty expose 8000 --hide              take the URL down again\n\n\
                  Prints the URL on stdout and nowhere else. Without --public it carries an access \
                  token (`_token`): treat the whole URL like a password. The URL stays the same for \
                  this sandbox and port, across stops and resumes. boatparty does not keep it in \
                  its state or logs; run the command again to print it again. Ports the provider \
                  uses (22, 6080, 8090, 8911, 47984 to 48010) are refused. The service must listen \
                  on 0.0.0.0, or on 127.0.0.1 with --localhost. --hide takes the URL down (the \
                  server keeps running; the access token is kept, so exposing the port again \
                  brings the same URL back).")]
pub struct ExposeArgs {
    /// The sandbox port the service listens on.
    #[arg(value_name = "PORT", value_parser = crate::transport::forward::parse_expose_port)]
    pub port: u16,

    /// A URL without an access token: anyone who has it can open it.
    #[arg(long)]
    pub public: bool,

    /// The service listens on the sandbox's 127.0.0.1 only.
    #[arg(long)]
    pub localhost: bool,

    /// A display title for the exposed port.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Take the port's URL down instead.
    #[arg(long, conflicts_with_all = ["public", "localhost", "title"])]
    pub hide: bool,
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
    /// boatparty's own starts in the rolling minute, hour and day windows,
    /// per project, beside the account's totals from the provider (the gap
    /// is starts made elsewhere, such as CI or other tools); each start's
    /// time; and when the windows free up over the coming hours.
    Starts,
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
    after_help = "The key never appears in an argument list or in shell history:\n  \
                  boatparty login --create-key     create a key with every permission boatparty needs, \
                  through the signed-in boat CLI, and store it\n  \
                  ... | boatparty login --key-stdin     store a key you already have, read from a pipe"
)]
pub struct LoginArgs {
    /// Read the API key from standard input.
    #[arg(
        long,
        required_unless_present = "create_key",
        conflicts_with = "create_key"
    )]
    pub key_stdin: bool,

    /// Create a key with every permission boatparty needs (90 days) through
    /// the signed-in boat CLI (`boat api-key create`), and store it. The key
    /// passes from boat to boatparty through a pipe only.
    #[arg(long)]
    pub create_key: bool,

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
    after_help = "Examples:\n  boatparty shell                 interactive shell in the checkout\n  \
                  boatparty shell -- uname -a     run one command; exits with its status\n\
                  Each word after -- is quoted, so `boatparty shell -- sh -c 'a | b'` runs exactly that."
)]
pub struct ShellArgs {
    /// Use the project's exclusive gate member.
    #[arg(long)]
    pub gate: bool,
    /// The command to run, after `--`. Without one, an interactive shell opens.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:\n  \
                  boatparty fetch target/release/app            into the current directory\n  \
                  boatparty fetch target/release/app /tmp/app-linux   under a new name\n  \
                  boatparty fetch /tmp/report out/              an absolute path, into out/\n\n\
                  A relative PATH is resolved in the sandbox's checkout (/home/user/<project>, or [sandbox] remote_path). \
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
pub struct StopArgs {
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
#[command(
    after_help = "The base is optional. Without one, every new sandbox installs and builds \
                  dependencies on its first command; with one, it starts from the saved snapshot \
                  bp-<project> and its first sync sends only what changed since the base \
                  commit. Sandboxes that already exist are not affected by a new base. Set \
                  [base] auto_refresh = true to refresh stale bases in the background after create. \
                  `base status` reports freshness and refresh activity."
)]
pub struct BaseArgs {
    #[command(subcommand)]
    pub action: BaseAction,
}

#[derive(Debug, Subcommand)]
pub enum BaseAction {
    /// Build the base: warm the builder sandbox at the base ref and save it.
    #[command(
        after_help = "Brings up the project's builder sandbox (bp-<project>-builder: never \
                      leased to a workspace, not counted against max_sandboxes, stopped between \
                      builds so a rebuild is incremental), syncs the base ref (`[base] ref`; by \
                      default the default remote branch, origin/HEAD or origin/main or \
                      origin/master, when the repository has one, else HEAD) in the project's \
                      history mode, runs the setup and ensure steps and `[base] warm`, settles \
                      the checkout, waits until the builder's files are complete, stops it and \
                      saves it as the saved snapshot bp-<project>. The base is recorded only \
                      once the save is ready, so a failed build leaves the previous base in \
                      use. The builder gets no account secrets (noEnv) and no credential is \
                      written to its disk."
    )]
    Build,
    /// Show the base: its commit, age, commits behind its ref, changed
    /// stale inputs, size, and the account's saved-snapshot allowance.
    Status,
    /// Delete the base's saved snapshot, its record and the builder sandbox.
    Rm(BaseRmArgs),
}

#[derive(Debug, Args)]
pub struct BaseRmArgs {
    /// Keep the stopped builder sandbox, so a later build is incremental.
    #[arg(long)]
    pub keep_builder: bool,
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

    fn run(args: &[&str]) -> Result<RunArgs, clap::Error> {
        let mut argv = vec!["boatparty", "run"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Command::Run(args) => args,
            other => panic!("parsed {other:?}"),
        })
    }

    fn serve(args: &[&str]) -> Result<ServeArgs, clap::Error> {
        let mut argv = vec!["boatparty", "serve"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Command::Serve(args) => args,
            other => panic!("parsed {other:?}"),
        })
    }

    #[test]
    fn cruise_takes_forwards_and_a_command_in_the_cheers_forms() {
        let args = serve(&[
            "--forward",
            "8000:8000",
            "--forward",
            "0.0.0.0:9000:9001",
            "--reverse",
            "20000:7777",
            "--ready-port",
            "8000",
            "--",
            "python3",
            "-m",
            "http.server",
            "8000",
        ])
        .unwrap();
        assert_eq!(args.forward.len(), 2);
        assert_eq!(args.forward[0].bind, "127.0.0.1");
        assert_eq!(args.forward[1].bind, "0.0.0.0");
        assert_eq!(args.reverse[0].listen, 20000);
        assert_eq!(args.ready_port, Some(8000));
        assert_eq!(args.max_minutes, 120.0);
        assert_eq!(args.command, ["python3", "-m", "http.server", "8000"]);

        let named = serve(&["--max-minutes", "0", "dev", "--port", "3000"]).unwrap();
        assert_eq!(named.words, ["dev", "--port", "3000"]);
        assert_eq!(named.max_minutes, 0.0);
        let line = serve(&["--sh", "npm start"]).unwrap();
        assert_eq!(line.command_args().sh.as_deref(), Some("npm start"));

        assert!(serve(&[]).is_err());
        assert!(serve(&["--forward", "8000", "--", "x"]).is_err());
        assert!(serve(&["--ready-port", "0", "--", "x"]).is_err());
        assert!(serve(&["--max-minutes", "-1", "--", "x"]).is_err());
    }

    #[test]
    fn expose_refuses_reserved_ports_and_forward_parses_its_ports() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["boatparty"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| cli.command)
        };
        match parse(&["expose", "8000", "--title", "app"]).unwrap() {
            Command::Expose(args) => {
                assert_eq!(args.port, 8000);
                assert!(!args.public && !args.localhost);
                assert_eq!(args.title.as_deref(), Some("app"));
            }
            other => panic!("parsed {other:?}"),
        }
        for reserved in ["22", "6080", "8090", "8911", "47984", "48010"] {
            let error = parse(&["expose", reserved]).unwrap_err();
            assert_eq!(error.exit_code(), 2);
            assert!(error.to_string().contains("reserved"), "{error}");
        }
        match parse(&["forward", "5432:15432", "--bind", "0.0.0.0"]).unwrap() {
            Command::Forward(args) => {
                assert_eq!((args.ports.remote, args.ports.local), (5432, 15432));
                assert_eq!(args.bind.as_deref(), Some("0.0.0.0"));
            }
            other => panic!("parsed {other:?}"),
        }
        assert!(parse(&["forward", "1:2:3"]).is_err());
        assert!(parse(&["forward", "8000", "--bind", "example.com"]).is_err());
    }

    #[test]
    fn cheers_takes_a_name_with_arguments_a_quoted_argv_or_a_shell_line() {
        let named = run(&["test", "-q", "--nocapture"]).unwrap();
        assert_eq!(named.words, ["test", "-q", "--nocapture"]);
        assert!(named.command.is_empty() && named.sh.is_none());
        let argv = run(&["--", "cargo", "test"]).unwrap();
        assert!(argv.words.is_empty());
        assert_eq!(argv.command, ["cargo", "test"]);
        let line = run(&["--sh", "a | b"]).unwrap();
        assert_eq!(line.sh.as_deref(), Some("a | b"));
        // After NAME everything is passed on, `--` included.
        let both = run(&["test", "--", "--nocapture"]).unwrap();
        assert_eq!(both.words, ["test", "--", "--nocapture"]);
        assert!(both.command.is_empty());
        assert!(run(&[]).is_err());
        assert!(run(&["--sh", "x", "--", "y"]).is_err());
        assert!(run(&["--sh", "x", "test"]).is_err());
    }
}
