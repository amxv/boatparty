pub mod base;
mod checkout;
pub mod config;
pub mod desktop;
pub mod doctor;
pub mod env;
pub mod expose;
pub mod fetch;
pub mod forward;
pub mod gate;
pub mod init;
pub mod ledger;
pub mod limits;
pub mod log;
pub mod login;
pub mod pin;
pub mod pipe;
pub mod pool;
pub mod preset;
pub mod rm;
pub mod run;
pub mod serve;
mod session;
pub mod shell;
pub mod skill;
pub mod start;
pub mod status;
pub mod stop;
pub mod sync;
pub mod url;

use crate::Context;
use crate::account::{self, ApiKey, KeySource, LoadedAccount, ProjectSettings};
use crate::error::AppError;
use crate::project::{ProjectConfig, Workspace};
use crate::provider::{self, Provider};

/// Load `account.toml`, saying so when this call created it.
pub(crate) fn load_account(ctx: &Context) -> Result<LoadedAccount, AppError> {
    let loaded = account::load_or_create(&ctx.paths)?;
    if loaded.created {
        ctx.out.note(format_args!(
            "created {} with default settings",
            loaded.path.display()
        ));
    }
    Ok(loaded)
}

/// The active key, or an error that says how to get one.
pub(crate) fn require_key(ctx: &Context) -> Result<(ApiKey, KeySource), AppError> {
    account::credentials::load(&ctx.paths)?.ok_or_else(|| {
        AppError::credentials(format!(
            "no boat API key is stored; create and store one with:\n  {}",
            provider::credential_help().create_and_login_command
        ))
    })
}

/// Open the provider with the active key.
pub(crate) fn open_provider(ctx: &Context) -> Result<Box<dyn Provider>, AppError> {
    let loaded = load_account(ctx)?;
    let (key, _) = require_key(ctx)?;
    provider::connect(loaded.config.provider.api_url.as_deref(), key)
}

/// The runner on `member` over SSH, with the project's spec and the
/// provider's signed upload for large packs when a provider can be opened
/// (it is only called when a pack is that large, so the warm path stays
/// free of provider calls).
pub(crate) fn runner_host(
    ctx: &Context,
    target: &crate::transport::ssh::SshTarget,
    member: &crate::state::Member,
    spec: crate::runner::RunnerSpec,
) -> crate::runner::SshRunner {
    let upload = member.id.clone().and_then(|sandbox_id| {
        let provider = open_provider(ctx).ok()?;
        Some(crate::runner::SignedUpload {
            provider,
            sandbox_id,
            threshold: crate::runner::upload_threshold(),
        })
    });
    crate::runner::SshRunner {
        target: target.clone(),
        upload,
        spec: Some(spec.with_restart(member.recovery.snapshot_seconds)),
        dedicated: None,
    }
}

/// Everything a workspace command needs: the provider, the workspace, its
/// project and the project's limits.
pub(crate) struct WorkspaceContext {
    pub provider: Box<dyn Provider>,
    pub workspace: Workspace,
    pub project: ProjectConfig,
    /// The project's limits, with what its boatparty.toml asks for applied.
    pub settings: ProjectSettings,
}

impl WorkspaceContext {
    pub fn open(ctx: &Context) -> Result<Self, AppError> {
        let workspace = Workspace::current()?;
        let project = workspace.load_project(&ctx.paths)?;
        Self::for_project(ctx, workspace, project)
    }

    pub(crate) fn for_project(
        ctx: &Context,
        workspace: Workspace,
        project: ProjectConfig,
    ) -> Result<Self, AppError> {
        let mut account = load_account(ctx)?;
        if !account.config.projects.contains_key(project.name()) {
            account::config::register_project(&ctx.paths, project.name())?;
            ctx.out.note(format_args!(
                "registered project {} in {}",
                project.name(),
                account.path.display()
            ));
            account = account::load_or_create(&ctx.paths)?;
        }
        let settings = ProjectSettings::resolve(&account.config, project.name())
            .with_requested_machine(
                project.name(),
                project.requested_machine(),
                &workspace.project_file(),
                &account.path,
            )?;
        let (key, _) = require_key(ctx)?;
        let provider = provider::connect(account.config.provider.api_url.as_deref(), key)?;
        Ok(Self {
            provider,
            workspace,
            project,
            settings,
        })
    }

    pub fn pool<'a>(&'a self, ctx: &'a Context) -> crate::pool::Pool<'a> {
        let mut pool = crate::pool::Pool::new(
            &ctx.paths,
            self.provider.as_ref(),
            ctx.out,
            self.project.name().to_string(),
            self.workspace.clone(),
            crate::pool::PoolLimits::from_settings(&self.settings),
        );
        pool.auto_refresh = self.project.config().base.auto_refresh;
        pool.environment = self.project.environment().map(str::to_string);
        pool
    }

    /// How `start` and `shell` bring a member up.
    pub fn bring_up_options(&self, extend_running: bool) -> crate::pool::BringUp {
        crate::pool::BringUp {
            size: self.settings.machine,
            ttl_seconds: self.settings.lease_seconds(),
            extend_running,
        }
    }
}
