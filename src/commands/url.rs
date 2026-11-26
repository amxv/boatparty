//! Local discovery only. A killed serve disappears with its forward lock.
use serde::Serialize;
use serde_json::json;

use crate::Context;
use crate::error::AppError;
use crate::pool::service;
use crate::project::Workspace;
use crate::state::{ServiceForward, Store};

#[derive(Serialize)]
struct Forward<'a> {
    #[serde(flatten)]
    forward: &'a ServiceForward,
    pid: u32,
    started_at: u64,
}

pub fn run(ctx: &Context, port: Option<u16>) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project(&ctx.paths)?;
    let store = Store::new(&ctx.paths);
    let key = workspace.key();
    let name = project.name();
    let state = store.load()?;
    let mut forwards = Vec::new();
    let mut expired = false;
    if let Some(member) = state
        .project(name)
        .and_then(|pool| pool.affinity_member(&key))
    {
        for lease in &member.leases {
            let Some(serve) = &lease.service else {
                continue;
            };
            if !service::held(&ctx.paths, lease) {
                expired = true;
                continue;
            }
            if serve.started
                && lease
                    .workspace
                    .as_ref()
                    .is_none_or(|workspace| workspace == &key)
            {
                for forward in &serve.forwards {
                    if port.is_none_or(|port| port == forward.remote_port) {
                        forwards.push(Forward {
                            forward,
                            pid: lease.pid,
                            started_at: lease.started_at,
                        });
                    }
                }
            }
        }
    }
    if expired {
        store.update(|state| {
            if let Some(member) = state
                .project_mut(name)
                .members
                .iter_mut()
                .find(|m| m.affinity.as_ref() == Some(&key))
            {
                for lease in &mut member.leases {
                    if lease.service.is_some() && !service::held(&ctx.paths, lease) {
                        // A live client may still be finishing copy-back.
                        lease.service = None;
                    }
                }
            }
            Ok(())
        })?;
    }
    if forwards.is_empty() {
        let which = port.map_or(String::new(), |p| format!(" for remote port {p}"));
        return Err(AppError::new(
            "no_serve_forward",
            format!("no running serve forward{which} in this workspace"),
        ));
    }
    if ctx.out.is_json() {
        ctx.out
            .document(&json!({"ok":true,"project":name,"workspace":key,"forwards":forwards}));
    } else {
        for row in forwards {
            ctx.out.text(&row.forward.url);
        }
    }
    Ok(0)
}
