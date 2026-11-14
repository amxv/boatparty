//! `boatparty login --key-stdin`: store the provider API key.

use std::io::{IsTerminal, Read};

use serde_json::json;

use super::load_account;
use crate::Context;
use crate::account::{ApiKey, KEY_ENV, credentials};
use crate::cli::LoginArgs;
use crate::error::AppError;
use crate::provider::{self, ErrorKind, ScopeDenial};

/// No API key is anywhere near this long; reading stops here.
const MAX_INPUT_BYTES: u64 = 4096;

pub fn run(ctx: &Context, args: &LoginArgs) -> Result<u8, AppError> {
    debug_assert!(args.key_stdin, "clap requires --key-stdin");
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(AppError::credentials(format!(
            "--key-stdin reads the key from a pipe so it is never shown or kept in shell history; for example:\n  {}",
            provider::credential_help().create_and_login_command
        )));
    }
    let mut raw = String::new();
    (&mut stdin)
        .take(MAX_INPUT_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| AppError::io("cannot read the key from standard input", error))?;
    if raw.len() as u64 > MAX_INPUT_BYTES {
        return Err(AppError::credentials(
            "standard input is too long to be an API key",
        ));
    }
    let key = ApiKey::parse(&raw)?;

    let account = load_account(ctx)?;
    let mut warning = None;
    let verified = if args.no_verify {
        false
    } else {
        let provider = provider::connect(account.config.provider.api_url.as_deref(), key.clone())?;
        match provider.limits() {
            Ok(_) => true,
            Err(error) => match error.kind() {
                ErrorKind::Unauthorized => {
                    return Err(AppError::credentials(
                        "the provider rejected this API key; nothing was stored",
                    ));
                }
                ErrorKind::ScopeForbidden(ScopeDenial::Expired) => {
                    return Err(AppError::credentials(
                        "this API key has expired; nothing was stored",
                    ));
                }
                ErrorKind::ScopeForbidden(_) => {
                    warning = Some(format!(
                        "the key is valid but cannot read account limits ({error}); run `boatparty doctor`"
                    ));
                    true
                }
                _ => {
                    return Err(AppError::from(error).with_hint(
                        "nothing was stored; retry, or pass --no-verify to store the key unchecked",
                    ));
                }
            },
        }
    };

    let path = credentials::store(&ctx.paths, &key)?;
    let overridden = std::env::var_os(KEY_ENV).is_some_and(|value| !value.is_empty());

    if let Some(warning) = &warning {
        ctx.out.note(warning);
    }
    if overridden {
        ctx.out.note(format_args!(
            "{KEY_ENV} is set and overrides the stored key until it is unset"
        ));
    }
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "credentials_file": path,
            "verified": verified,
            "overridden_by_environment": overridden,
        }));
    } else {
        ctx.out.note(format_args!(
            "stored the API key in {} (mode 0600){}",
            path.display(),
            if verified {
                "; the provider accepted it"
            } else {
                ""
            }
        ));
        ctx.out
            .note("run `boatparty doctor` to check that the key allows everything boatparty does");
    }
    Ok(0)
}
