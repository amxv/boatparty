pub mod board;
pub mod cheers;
pub mod dock;
pub mod doctor;
pub mod fetch;
pub mod init;
pub mod launch;
pub mod limits;
pub mod load;
pub mod log;
pub mod login;
pub mod pin;
pub mod pool;
pub mod rm;
mod session;
pub mod status;

use crate::Context;
use crate::account::{self, ApiKey, KeySource, LoadedAccount};
use crate::error::AppError;
use crate::project::{ProjectConfig, Workspace};
use crate::provider::{self, MachineSize, Provider};

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

/// The runner on `member` over SSH, with the provider's signed upload for
/// large packs when a provider can be opened (it is only called when a pack
/// is that large, so the warm path stays free of provider calls).
pub(crate) fn runner_host(
    ctx: &Context,
    target: &crate::transport::ssh::SshTarget,
    member: &crate::state::Member,
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
    }
}

/// Everything a workspace command needs: the account, the provider, the
/// workspace and its project.
pub(crate) struct WorkspaceContext {
    pub account: LoadedAccount,
    pub provider: Box<dyn Provider>,
    pub workspace: Workspace,
    pub project: ProjectConfig,
}

impl WorkspaceContext {
    pub fn open(ctx: &Context) -> Result<Self, AppError> {
        let workspace = Workspace::current()?;
        let project = workspace.load_project()?;
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
        let (key, _) = require_key(ctx)?;
        let provider = provider::connect(account.config.provider.api_url.as_deref(), key)?;
        Ok(Self {
            account,
            provider,
            workspace,
            project,
        })
    }

    pub fn pool<'a>(&'a self, ctx: &'a Context) -> crate::pool::Pool<'a> {
        crate::pool::Pool::new(
            &ctx.paths,
            self.provider.as_ref(),
            ctx.out,
            self.project.name().to_string(),
            self.workspace.clone(),
            crate::pool::PoolLimits::for_project(&self.account.config, self.project.name()),
        )
    }

    /// How `launch` and `board` bring a member up.
    pub fn bring_up_options(&self, extend_running: bool) -> crate::pool::BringUp {
        let defaults = &self.account.config.defaults;
        crate::pool::BringUp {
            size: defaults.machine.parse().unwrap_or(MachineSize::Default),
            ttl_seconds: u64::from(defaults.lease_minutes) * 60,
            extend_running,
        }
    }
}
