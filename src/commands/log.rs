//! `boatparty log`: recent runs from `runs.jsonl`, newest last.

use serde_json::{Value, json};

use crate::Context;
use crate::cli::LogArgs;
use crate::error::AppError;
use crate::runs;

pub fn run(ctx: &Context, args: &LogArgs) -> Result<u8, AppError> {
    let records = runs::read(&ctx.paths, args.limit)?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({ "ok": true, "runs": records }));
        return Ok(0);
    }
    if records.is_empty() {
        ctx.out.text(format_args!(
            "no runs recorded yet in {}",
            ctx.paths.runs_file().display()
        ));
        return Ok(0);
    }
    for record in &records {
        ctx.out.text(line(record));
    }
    Ok(0)
}

/// One run as a line: when, outcome, time, member, command.
fn line(record: &Value) -> String {
    let text = |key: &str| record[key].as_str().unwrap_or("-").to_string();
    let outcome = match (&record["exit_code"], &record["error"]) {
        (Value::Number(code), _) if record["cancelled"] == json!(true) => {
            format!("cancelled ({code})")
        }
        (Value::Number(code), _) => format!("exit {code}"),
        (_, Value::String(error)) => format!("failed: {error}"),
        _ => "cancelled".to_string(),
    };
    let seconds = record["seconds"]
        .as_f64()
        .map_or("-".to_string(), |s| format!("{s:.1}s"));
    let prep = record["prep_seconds"]
        .as_f64()
        .map(|s| format!(" (prep {s:.1}s)"))
        .unwrap_or_default();
    format!(
        "{}  {:<14} {seconds}{prep}  {} {}  {}",
        text("at"),
        outcome,
        text("project"),
        text("member"),
        text("command")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_reads_as_one_line() {
        let record = json!({
            "at": "2026-11-15T10:00:00Z", "exit_code": 7, "seconds": 1.31,
            "prep_seconds": 0.4, "project": "app", "member": "bp-app-1",
            "command": "cargo test"
        });
        assert_eq!(
            line(&record),
            "2026-11-15T10:00:00Z  exit 7         1.3s (prep 0.4s)  app bp-app-1  cargo test"
        );
        let failed = json!({"at": "x", "exit_code": null, "error": "connection", "seconds": 2.0});
        assert!(line(&failed).contains("failed: connection"));
    }
}
