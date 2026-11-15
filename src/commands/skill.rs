//! `boatparty skill`: the skill that teaches a coding agent to use
//! boatparty, written to the state directory on demand.
//!
//! The text lives in the repository as `skill/SKILL.md` and is embedded in
//! the binary, with this binary's version filled in. Nothing is installed:
//! the command prints where the file is and says to copy it into an agent's
//! skills directory only if the user asks.

use serde_json::json;

use crate::Context;
use crate::cli::SkillArgs;
use crate::error::AppError;

/// The skill as written in the repository.
const SOURCE: &str = include_str!("../../skill/SKILL.md");

/// The skill's text, naming this binary's version.
pub fn text() -> String {
    SOURCE.replace("{{version}}", env!("CARGO_PKG_VERSION"))
}

pub fn run(ctx: &Context, args: &SkillArgs) -> Result<u8, AppError> {
    let text = text();
    let version = env!("CARGO_PKG_VERSION");
    if args.print {
        if ctx.out.is_json() {
            ctx.out.document(&json!({
                "ok": true,
                "version": version,
                "content": text,
            }));
        } else {
            ctx.out.text(text.trim_end());
        }
        return Ok(0);
    }
    let dir = ctx.paths.skill_dir();
    let path = dir.join("SKILL.md");
    std::fs::create_dir_all(&dir)
        .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))?;
    let partial = dir.join(format!(".SKILL.md.{}", std::process::id()));
    std::fs::write(&partial, &text)
        .and_then(|()| std::fs::rename(&partial, &path))
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "path": path.display().to_string(),
            "version": version,
            "bytes": text.len(),
        }));
    } else {
        ctx.out.text(path.display());
    }
    ctx.out.note(format_args!(
        "this skill teaches coding agents to use boatparty; copy {} into an agent's skills directory only if the user asks for it",
        dir.display()
    ));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_names_this_version_and_has_frontmatter() {
        let text = text();
        assert!(text.starts_with("---\nname: boatparty\ndescription: "));
        assert!(text.contains(&format!("boatparty {}", env!("CARGO_PKG_VERSION"))));
        assert!(!text.contains("{{version}}"));
        assert!(!text.contains('\u{2014}'), "no em dashes");
    }
}
