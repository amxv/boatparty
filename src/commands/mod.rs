pub mod doctor;
pub mod limits;
pub mod login;

use crate::Context;
use crate::account::{self, ApiKey, KeySource, LoadedAccount};
use crate::error::AppError;
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
