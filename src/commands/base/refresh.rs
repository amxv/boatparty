//! A detached staleness check after a successful create from a saved base.
use std::io::Write;
use std::process::{Command, Stdio};

use crate::base::{self, BaseRecord};
use crate::commands::WorkspaceContext;
use crate::pool::Pool;
use crate::state::Store;
use crate::{Context, error::AppError, system};

pub fn spawn(pool: &Pool<'_>, built_at: u64) -> Result<(), AppError> {
    let exe = std::env::current_exe().map_err(|e| AppError::io("locating boatparty", e))?;
    let mut cmd = Command::new(exe);
    cmd.args(["refresh-base", &pool.project, &built_at.to_string()])
        .current_dir(&pool.workspace.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    system::separate_process_group(&mut cmd);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x208);
    cmd.spawn()
        .map_err(|e| AppError::io("starting the base refresh helper", e))?;
    Ok(())
}

pub fn status(ctx: &Context, project: &str) -> Result<(bool, Option<String>), AppError> {
    let state = Store::new(&ctx.paths).load()?;
    let Some(slot) = state.project(project) else {
        return Ok((false, None));
    };
    let active = slot.refresh_pid.is_some_and(system::pid_alive);
    let error = if slot.refresh_pid.is_some() && !active {
        Some("interrupted".into())
    } else {
        slot.last_refresh_error.clone()
    };
    Ok((active, error))
}

pub fn run(ctx: &Context, project: &str, built_at: u64) -> Result<u8, AppError> {
    crate::project::validate_name(project).map_err(AppError::config)?;
    let _guard = match super::lock_base(ctx, project) {
        Ok(lock) => lock,
        Err(error) if error.code() == "base_busy" => return Ok(0),
        Err(error) => return Err(error),
    };
    let store = Store::new(&ctx.paths);
    let pid = std::process::id();
    let claimed = store.update(|state| {
        let slot = state.project_mut(project);
        let record = base::record(slot).filter(|r| r.built_at == built_at);
        if record.is_some() {
            slot.refresh_pid = Some(pid);
        }
        Ok(record)
    })?;
    let Some(record) = claimed else { return Ok(0) };
    let result = check(ctx, project, &record);
    store.update(|state| {
        let slot = state.project_mut(project);
        if slot.refresh_pid == Some(pid) {
            slot.refresh_pid = None;
            match &result {
                Ok(true) => slot.last_refresh_error = None,
                Err(err) => slot.last_refresh_error = Some(err.code().into()),
                Ok(false) => (),
            }
        }
        Ok(())
    })?;
    result.map(|_| 0)
}

fn check(ctx: &Context, project: &str, record: &BaseRecord) -> Result<bool, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    if wctx.project.name() != project
        || !wctx.project.config().base.auto_refresh
        || !super::assess(&wctx.project, &wctx.workspace.root, record)?.stale
    {
        return Ok(false);
    }
    let path = ctx
        .paths
        .state_dir()
        .join(format!("base-refresh-{project}.log"));
    let mut log = system::private_file()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| AppError::io("opening base refresh log", e))?;
    writeln!(
        log,
        "{} refreshing {}",
        crate::runs::rfc3339(system::now_ms()),
        record.commit
    )
    .map_err(|e| AppError::io("writing base refresh log", e))?;
    let interrupts = crate::signals::Interrupts::install()?;
    let built = super::build::build(ctx, &wctx, &interrupts);
    // Log lifecycle only. Commands and their output may contain secrets.
    let outcome = match &built {
        Ok(built) => format!("saved {}", built.record.commit),
        Err(err) => format!("failed ({})", err.code()),
    };
    writeln!(log, "{} {outcome}", crate::runs::rfc3339(system::now_ms()))
        .map_err(|e| AppError::io("writing base refresh log", e))?;
    built.map(|_| true)
}

pub(crate) fn print(ctx: &Context, active: bool, error: Option<&str>) {
    if active {
        ctx.out.text("refreshing the base in the background");
    }
    if let Some(code) = error {
        ctx.out
            .text(format_args!("last background base refresh failed ({code})"));
    }
}
