//! Check one commit on the exclusive gate member. The checkout and its
//! project settings both come from Git objects in the selected revision.
use std::io::Write;

use serde_json::json;

use super::WorkspaceContext;
use crate::base::reference;
use crate::cli::GateArgs;
use crate::config::{
    self,
    presets::{self, PresetDirs, PresetFile, Source},
};
use crate::pool::{
    self,
    lease::{self, Heartbeat, LeaseTimes},
};
use crate::project::{PROJECT_FILE, ProjectConfig, Workspace};
use crate::runner::RunnerSpec;
use crate::signals::Interrupts;
use crate::state::{Member, SyncState};
use crate::stream::Stream;
use crate::sync::{self, MirrorRequest, RunCredentials, RunEnd, Sink, Snapshot, WorktreeInfo};
use crate::transport::ssh::{SshTarget, shell_quote};
use crate::{Context, error::AppError, system};

fn revision(args: &GateArgs, info: &WorktreeInfo) -> Result<String, AppError> {
    if args.default_branch {
        let name = reference::default_remote_branch(info)
            .or_else(|| {
                ["refs/heads/main", "refs/heads/master"]
                    .into_iter()
                    .find(|name| reference::commit_of(info, name).is_some())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                AppError::config(
                    "cannot resolve the default branch locally; fetch it or use --commit REV",
                )
            })?;
        return reference::commit_of(info, &name)
            .ok_or_else(|| AppError::config("default branch disappeared"));
    }
    let rev = args.commit.as_deref().unwrap_or("HEAD");
    reference::commit_of(info, rev).ok_or_else(|| {
        AppError::config(format!(
            "gate revision {rev:?} does not name a local commit"
        ))
    })
}

fn committed_project(ctx: &Context, info: &WorktreeInfo) -> Result<ProjectConfig, AppError> {
    let blob = |path: &str| {
        let object = format!("{}:{path}", info.head);
        let out = sync::git(&info.root, &["cat-file", "blob", &object], &[], None, true)?;
        if !out.status.success() {
            return Ok(None);
        }
        String::from_utf8(out.stdout)
            .map(Some)
            .map_err(|_| AppError::config(format!("{object} contains invalid UTF-8")))
    };
    let text = blob(PROJECT_FILE)?.ok_or_else(|| {
        AppError::config(format!("{} has no committed {PROJECT_FILE}", info.head))
    })?;
    let dirs = PresetDirs::new(ctx.paths.user_presets_dir(), None);
    let resolved = config::resolve_with(&text, &info.root.join(PROJECT_FILE), |name| {
        presets::valid_name(name)?;
        let path = format!("{}/{name}.toml", presets::PROJECT_PRESETS_DIR);
        if let Some(text) = blob(&path).map_err(|e| e.to_string())? {
            return Ok(PresetFile {
                name: name.into(),
                source: Source::Project(info.root.join(path)),
                text,
            });
        }
        config::load_preset(name, &dirs)
    })?;
    Ok(ProjectConfig { resolved })
}

pub fn run(ctx: &Context, args: &GateArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let mut info = WorktreeInfo::read(&workspace.root)?;
    info.head = revision(args, &info)?;
    info.branch = None;
    let project = committed_project(ctx, &info)?;
    let line = if let Some((name, tail)) = args.words.split_first() {
        project.named_command(name, tail)?
    } else if !args.command.is_empty() {
        args.command
            .iter()
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        project.config().gate.command.clone().ok_or_else(|| AppError::config("set [gate] command in the selected commit, or supply a named command or -- COMMAND"))?
    };
    let wctx = WorkspaceContext::for_project(ctx, workspace, project)?;
    let pool = wctx.pool(ctx);
    let (member, target, warm) = match pool::warm_gate(&pool)? {
        Some((member, target)) => (member, target, true),
        None => {
            let up = pool::bring_up_gate(&pool, wctx.bring_up_options(false))?;
            (up.member, up.target, false)
        }
    };
    let interrupts = Interrupts::install()?;
    let id = member.id.as_deref().expect("connected gate has a sandbox");
    let mut stream = Stream::new(ctx.out.is_json());
    let times = LeaseTimes::from_settings(&wctx.settings);
    let mut output = GateOutput {
        stream: &mut stream,
        ctx,
        id,
        times,
        heartbeat: None,
    };
    let result = execute(
        &wctx,
        &member,
        &target,
        &info,
        &line,
        &interrupts,
        &mut output,
    );
    drop(output);
    let project = wctx.project.name();
    lease::finish(
        &pool.store,
        project,
        id,
        |member| {
            member.last_used_at = system::now_ms();
            if let Ok((_, _, Some(sync))) = &result {
                member.sync = sync.clone();
            }
        },
        || {
            if let Err(err) = lease::spawn_idle_helper(project, id) {
                stream.note(format_args!("warning: {err}"));
            }
        },
    )?;
    match result {
        Ok((exit, patch, _)) => {
            if let Some(path) = &patch {
                stream.note(format_args!("gate changes saved to {}", path.display()));
            }
            stream.note(format_args!(
                "{} checked {} (exit {exit})",
                member.name, info.head
            ));
            stream.result(
                json!({"ok":true,"kind":"gate","project":project,"commit":info.head,
                "member":{"name":member.name,"id":id},"exit_code":exit,"patch":patch,"warm":warm}),
            );
            Ok(exit)
        }
        Err(err) => {
            stream.error(&err);
            Ok(err.exit_code())
        }
    }
}

type GateResult = (u8, Option<std::path::PathBuf>, Option<SyncState>);

#[allow(clippy::too_many_arguments)]
fn execute(
    wctx: &WorkspaceContext,
    member: &Member,
    target: &SshTarget,
    info: &WorktreeInfo,
    line: &str,
    interrupts: &Interrupts,
    output: &mut GateOutput<'_>,
) -> Result<GateResult, AppError> {
    let project = &wctx.project;
    let history = project.history();
    let snapshot = Snapshot {
        tree: sync::git_text(
            &info.root,
            &["rev-parse", &format!("{}^{{tree}}", info.head)],
        )?,
        wip: info.head.clone(),
    };
    let (refs, digest) = sync::mirrored_refs(info, history)?;
    let seed = project.seed_config();
    let req = MirrorRequest {
        project: project.name(),
        info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: member.sync.refs_digest.as_deref() != Some(&digest),
        known: sync::pack::usable_known(history, &member.sync.mode, &member.sync.known),
        history,
        seed: Some(&seed),
    };
    let credentials = RunCredentials::gather(
        &info.root,
        project.git_identity(),
        &project.run_token_source(),
    );
    let host = super::runner_host(
        output.ctx,
        target,
        member,
        RunnerSpec::new(project.config()),
    );
    let outcome = sync::mirror::gate_command(&host, &req, line, &credentials, output, interrupts)?;
    let RunEnd::Done {
        exit_code,
        patch_bytes,
        ..
    } = outcome.end
    else {
        if let Some(exit) = interrupts.exit_status() {
            return Ok((exit, None, None));
        }
        if matches!(outcome.end, RunEnd::Lost { status: Some(95) }) {
            return Err(AppError::new(
                "gate_busy",
                "another gate still holds the remote checkout",
            ));
        }
        return Err(AppError::new(
            "connection",
            "gate connection ended before its result arrived",
        ));
    };
    let patch = if patch_bytes > 0 {
        let fetched = sync::fetch_patch_routed(&host, project.name(), &outcome.nonce, patch_bytes)?;
        let path = info
            .git_dir
            .join(format!("boatparty-gate-{}.patch", outcome.nonce));
        system::private_file()
            .create_new(true)
            .write(true)
            .open(&path)
            .and_then(|mut f| f.write_all(&fetched.patch))
            .map_err(|e| AppError::io(format!("saving {}", path.display()), e))?;
        Some(path)
    } else {
        None
    };
    let state = SyncState {
        mode: history.as_str().into(),
        known: outcome.known,
        refs_digest: Some(digest),
    };
    Ok((
        interrupts
            .exit_status()
            .unwrap_or(u8::try_from(exit_code).unwrap_or(125)),
        patch,
        Some(state),
    ))
}

struct GateOutput<'a> {
    stream: &'a mut Stream,
    ctx: &'a Context,
    id: &'a str,
    times: LeaseTimes,
    heartbeat: Option<Heartbeat>,
}

impl GateOutput<'_> {
    fn keep_alive(&mut self) {
        self.heartbeat
            .get_or_insert_with(|| Heartbeat::for_member(&self.ctx.paths, self.id, self.times));
    }
}

impl Sink for GateOutput<'_> {
    fn stdout(&mut self, bytes: &[u8]) {
        self.stream.stdout(bytes)
    }
    fn stderr(&mut self, bytes: &[u8]) {
        self.stream.stderr(bytes)
    }
    fn note(&mut self, msg: &str) {
        self.stream.note(msg)
    }
    fn event(&mut self, name: &str) {
        if matches!(name, "preparing" | "started") {
            self.keep_alive()
        }
        self.stream.event(name);
    }
}
