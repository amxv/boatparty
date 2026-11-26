//! `boatparty expose`: a public HTTPS URL for a port of this workspace's
//! sandbox, gated by an access token unless `--public`.
//!
//! The URL (token included) is printed on stdout and nowhere else: it is
//! not written to state.json, runs.jsonl or any of the tool's log lines, and
//! the provider keeps it out of its errors. The provider answers with the
//! same URL every time for a sandbox and port, so it is never stored.

use serde_json::json;

use super::session::with_session;
use crate::Context;
use crate::cli::ExposeArgs;
use crate::error::AppError;
use crate::provider::HostPort;

pub fn run(ctx: &Context, args: &ExposeArgs) -> Result<u8, AppError> {
    if args.hide {
        return hide(ctx, args.port);
    }
    let request = HostPort {
        port: args.port,
        public: args.public,
        localhost: args.localhost,
        title: args.title.clone(),
    };
    let label = format!("expose {}", args.port);
    let ended = with_session(ctx, &label, |held| {
        let id = held
            .member
            .id
            .clone()
            .ok_or_else(|| AppError::new("member_pending", "the sandbox has no id yet"))?;
        let provider = super::open_provider(ctx)?;
        crate::pool::health::ensure_tunnel(
            ctx,
            held.project,
            held.member,
            held.target,
            provider.as_ref(),
        )?;
        let hosted = provider.host_port(&id, &request)?;
        Ok((
            hosted,
            held.member.name.clone(),
            id,
            held.project.to_string(),
        ))
    })?;
    let (hosted, member, id, project) = ended.value?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": project,
            "member": { "name": member, "id": id },
            "port": hosted.port,
            "url": hosted.url,
            "protected": hosted.protected,
            "access": if hosted.protected { "private" } else { "public" },
            "localhost": hosted.localhost,
        }));
    } else {
        ctx.out.text(&hosted.url);
    }
    let access = if hosted.protected {
        "the URL carries an access token: share it like a password"
    } else {
        "the URL is public: anyone who has it can open it"
    };
    ctx.out.note(format_args!(
        "exposed port {} of {member} ({id}); {access}",
        hosted.port
    ));
    if !hosted.localhost {
        ctx.out.note(format_args!(
            "the service must listen on 0.0.0.0:{} (for one bound to 127.0.0.1, add --localhost)",
            hosted.port
        ));
    }
    Ok(ended.interrupted.unwrap_or(0))
}

/// `expose --hide`: the sandbox's own `host hide <port>` (the provider has
/// no API for it), over SSH like `shell`.
fn hide(ctx: &Context, port: u16) -> Result<u8, AppError> {
    let label = format!("expose --hide {port}");
    let ended = with_session(ctx, &label, |held| {
        let (code, stderr) = held.target.run_quiet(&format!("host hide {port}"))?;
        if code != 0 {
            return Err(AppError::new(
                "hide_failed",
                format!(
                    "`host hide {port}` on {} failed (exit {code}): {stderr}",
                    held.member.name
                ),
            ));
        }
        Ok((
            held.member.name.clone(),
            held.member.id.clone().unwrap_or_default(),
            held.project.to_string(),
        ))
    })?;
    let (member, id, project) = ended.value?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": project,
            "member": { "name": member, "id": id },
            "port": port,
            "hidden": true,
        }));
    }
    ctx.out.note(format_args!(
        "took down the URL of port {port} on {member} ({id}); the server keeps running"
    ));
    Ok(ended.interrupted.unwrap_or(0))
}
