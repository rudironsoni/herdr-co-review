//! Where co-review keeps its state and worktrees on disk.
//!
//! Layout (Linux example, under `~/.local/state/co-review`):
//!
//! ```text
//! <base>/
//!   sessions/<slug>/        session dir: state.json, lock, agent-launch.json
//!   worktrees/<slug>/       the checked-out PR the agent works in
//! ```
//!
//! `<base>` can be overridden with `$CO_REVIEW_HOME` (used by tests and by
//! anyone who wants everything in one place).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

/// Environment variable that overrides the base directory.
pub const HOME_ENV: &str = "CO_REVIEW_HOME";
/// Environment variable pointing at the active session directory. `start` sets
/// this in both panes so `view`, `add-finding`, etc. find the session with no
/// arguments.
pub const SESSION_ENV: &str = "CO_REVIEW_SESSION";

/// Environment variable holding the absolute path of the co-review executable
/// that started the session. `start` sets this in both panes; agent-facing
/// instructions invoke co-review through it (`"$CO_REVIEW_BIN"`), so a session
/// never depends on PATH resolution.
pub const BIN_ENV: &str = "CO_REVIEW_BIN";

/// The absolute path of the running co-review executable. Session identity
/// requires the *exact* binary, so anything else (or a failure to resolve) is
/// a hard error rather than a fallback to a bare `co-review` name.
pub fn require_self_bin() -> Result<String> {
    let p = std::env::current_exe().context("resolving the co-review executable path")?;
    if !p.is_absolute() {
        return Err(anyhow!(
            "the co-review executable path is not absolute: {}",
            p.display()
        ));
    }
    Ok(p.to_string_lossy().into_owned())
}

/// The base directory for all co-review data.
pub fn base_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let proj = directories::ProjectDirs::from("dev", "herdr", "co-review")
        .ok_or_else(|| anyhow!("could not determine a home directory for co-review state"))?;
    // Prefer the XDG state dir; fall back to the data dir on platforms without one.
    Ok(proj
        .state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| proj.data_dir().to_path_buf()))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sessions"))
}

pub fn worktrees_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("worktrees"))
}

/// The session directory for a given slug (e.g. `owner-repo-123`).
pub fn session_dir(slug: &str) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(slug))
}

/// The worktree path for a given slug.
pub fn worktree_path(slug: &str) -> Result<PathBuf> {
    Ok(worktrees_dir()?.join(slug))
}

/// The user config file path (`~/.config/co-review/config.toml`).
pub fn config_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return Ok(PathBuf::from(dir).join("config.toml"));
    }
    let proj = directories::ProjectDirs::from("dev", "herdr", "co-review")
        .ok_or_else(|| anyhow!("could not determine a config directory for co-review"))?;
    Ok(proj.config_dir().join("config.toml"))
}

/// The session an internal pane process belongs to, taken strictly from
/// `$CO_REVIEW_SESSION`. Unlike [`resolve_session_dir`], this never guesses:
/// the pane environment is the declared transport for session identity, so a
/// missing, empty, or invalid value is a hard error — never a scan of the
/// sessions directory, never the sole existing session.
pub fn require_session_env() -> Result<PathBuf> {
    let value = std::env::var(SESSION_ENV).map_err(|_| {
        anyhow!(
            "${SESSION_ENV} is not set; this command only works inside a prepared co-review pane"
        )
    })?;
    if value.is_empty() {
        return Err(anyhow!(
            "${SESSION_ENV} is empty; refusing to guess a session"
        ));
    }
    let path = PathBuf::from(&value);
    if !path.join(crate::store::STATE_FILE).is_file() {
        return Err(anyhow!(
            "${SESSION_ENV} points at {value}, which has no session state"
        ));
    }
    Ok(path)
}

/// Resolve which session directory to operate on, for agent/human-facing
/// subcommands. Precedence: explicit `--session` path, then `$CO_REVIEW_SESSION`,
/// then — if exactly one session exists — that one.
pub fn resolve_session_dir(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        if !path.join(crate::store::STATE_FILE).is_file() {
            return Err(anyhow!("no co-review session found at {}", path.display()));
        }
        return Ok(path);
    }
    if let Some(dir) = std::env::var_os(SESSION_ENV) {
        let path = PathBuf::from(dir);
        if path.join(crate::store::STATE_FILE).is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "${} points at {}, which has no session state",
            SESSION_ENV,
            path.display()
        ));
    }
    // Last resort: if there is exactly one session on disk, use it.
    let sessions = sessions_dir()?;
    let mut found = Vec::new();
    if sessions.is_dir() {
        for entry in std::fs::read_dir(&sessions)
            .with_context(|| format!("reading {}", sessions.display()))?
        {
            let entry = entry?;
            if entry.path().join(crate::store::STATE_FILE).is_file() {
                found.push(entry.path());
            }
        }
    }
    match found.len() {
        1 => Ok(found.pop().unwrap()),
        0 => Err(anyhow!(
            "no co-review session found; pass --session <dir> or set ${}",
            SESSION_ENV
        )),
        n => Err(anyhow!(
            "{n} co-review sessions exist; pass --session <dir> or set ${} to pick one",
            SESSION_ENV
        )),
    }
}
