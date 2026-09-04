//! Private agent-launch transport.
//!
//! The resolved agent argv — including the full rendered review prompt — is
//! far too large to type into a Herdr pane (`herdr pane run` crosses the pane's
//! PTY input, where long payloads get truncated; see herdr issue #2862). It is
//! instead persisted as `agent-launch.json` next to the session state, and the
//! hidden `__launch-agent` subcommand reads it back and execs the argv
//! directly, without a shell.
//!
//! The spec contains *only* the argv. PATH, `CO_REVIEW_*`, and the working
//! directory are runtime/session concerns: they come from the pane environment
//! (and, for `CO_REVIEW_*`, are reasserted just before exec).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{require_self_bin, BIN_ENV, SESSION_ENV};

/// File name of the launch spec inside the session directory.
pub const FILE_NAME: &str = "agent-launch.json";

/// The private agent-launch specification: the fully resolved agent argv
/// (`AgentConfig::build_command` output), nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchSpec {
    pub argv: Vec<String>,
}

impl AgentLaunchSpec {
    pub fn new(argv: Vec<String>) -> Self {
        AgentLaunchSpec { argv }
    }

    fn validate(&self) -> Result<()> {
        if self.argv.is_empty() {
            bail!("{FILE_NAME} has an empty argv; cannot launch an agent");
        }
        if self.argv[0].is_empty() {
            bail!("{FILE_NAME} has an empty executable; cannot launch an agent");
        }
        Ok(())
    }
}

fn spec_path(session_dir: &Path) -> PathBuf {
    session_dir.join(FILE_NAME)
}

/// Persist the launch spec into the session directory.
pub fn write(session_dir: &Path, spec: &AgentLaunchSpec) -> Result<()> {
    spec.validate()?;
    let json = serde_json::to_vec_pretty(spec).context("serializing agent launch spec")?;
    crate::util::atomic_write(&spec_path(session_dir), &json)
}

/// Remove the launch spec, e.g. when a session is resumed with `--no-agent`.
pub fn remove(session_dir: &Path) -> Result<()> {
    let path = spec_path(session_dir);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// Read the launch spec from the session directory.
pub fn read(session_dir: &Path) -> Result<AgentLaunchSpec> {
    let path = spec_path(session_dir);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let spec: AgentLaunchSpec =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    spec.validate()?;
    Ok(spec)
}

/// Replace this process with the configured agent (Unix), or spawn it and
/// propagate its exit status (other platforms).
///
/// `session_dir` must come from `crate::paths::require_session_env`, i.e. the
/// strictly validated `$CO_REVIEW_SESSION`. Before handing control to the
/// agent we reassert that validated value and re-derive `CO_REVIEW_BIN` from
/// this very executable (which Herdr invoked by absolute path), so shell
/// startup code cannot leak wrong values into the agent. PATH is inherited
/// untouched: `AgentConfig.command` is argv, not shell syntax, and the agent
/// executable resolves against the pane's normal environment.
pub fn execute(session_dir: &Path, spec: &AgentLaunchSpec) -> Result<()> {
    spec.validate()?;
    let mut cmd = std::process::Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..])
        .env(SESSION_ENV, session_dir)
        .env(BIN_ENV, require_self_bin()?);
    exec_inner(&mut cmd)
}

#[cfg(unix)]
fn exec_inner(cmd: &mut std::process::Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns on failure; on success this process *is* the agent.
    Err(cmd.exec()).context("executing the agent")
}

#[cfg(not(unix))]
fn exec_inner(cmd: &mut std::process::Command) -> Result<()> {
    let status = cmd.status().context("executing the agent")?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_argv_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![
            "my-agent".to_string(),
            "two words".to_string(),
            "it's \"quoted\"".to_string(),
            "ünïcødé ✓".to_string(),
            "line one\nline two".to_string(),
            format!("embedded {0}mid-token{0}", "{prompt}"),
        ];
        let spec = AgentLaunchSpec::new(argv.clone());
        write(dir.path(), &spec).unwrap();
        assert_eq!(read(dir.path()).unwrap().argv, argv);
    }

    #[test]
    fn spec_serializes_to_argv_only() {
        let spec = AgentLaunchSpec::new(vec!["claude".to_string(), "go".to_string()]);
        let v: serde_json::Value = serde_json::to_value(&spec).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 1, "spec contains only argv: {obj:?}");
        assert!(obj.contains_key("argv"));
    }

    #[test]
    fn rejects_empty_argv() {
        let spec = AgentLaunchSpec::new(vec![]);
        assert!(write(&PathBuf::from("/nonexistent"), &spec).is_err());
        let dir = tempfile::tempdir().unwrap();
        crate::util::atomic_write(&dir.path().join(FILE_NAME), b"{\"argv\": []}").unwrap();
        assert!(read(dir.path()).is_err());
    }

    #[test]
    fn read_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_err());
    }

    #[test]
    fn remove_deletes_spec_and_tolerates_absence() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &AgentLaunchSpec::new(vec!["a".to_string()])).unwrap();
        remove(dir.path()).unwrap();
        assert!(!dir.path().join(FILE_NAME).exists());
        remove(dir.path()).unwrap();
    }
}
