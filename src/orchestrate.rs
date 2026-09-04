//! `co-review start <pr>` — the orchestrator.
//!
//! It resolves the PR, checks it out into an isolated worktree, writes the
//! session state, and asks Herdr to lay out the split-screen: the agent in the
//! left pane and the navigator (`co-review view`) in the right pane.
//!
//! `--dry-run` makes this a fully offline preview: it prints the git and Herdr
//! commands it *would* run (and the exact prompt) without touching the network,
//! Herdr, or the filesystem. Real runs execute everything.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::agent_launch::AgentLaunchSpec;
use crate::cli::StartArgs;
use crate::config::Config;
use crate::git::{self, Git};
use crate::herdr::{shell_join, shell_quote, Herdr, PluginContext};
use crate::model::{PrInfo, SessionMeta, State};
use crate::store::Store;

/// The concrete plan for laying out the Herdr panes. Pure data so it can be
/// unit-tested and printed for `--dry-run`.
///
/// The two channels stay separate: session and binary identity travel through
/// Herdr's native `--env` (which never touches the pane's PTY input), while the
/// typed pane commands carry *only* the operation to execute — `<self_bin>
/// view` and `<self_bin> __launch-agent`. Nothing else (no env prefix, no
/// PATH, no session path, no prompt) ever crosses `pane run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    pub label: String,
    pub worktree: String,
    /// Exactly `CO_REVIEW_SESSION` and `CO_REVIEW_BIN`, set on both panes.
    pub env: Vec<(String, String)>,
    /// argv for the agent pane: `[self_bin, __launch-agent]`; `None` with
    /// `--no-agent`.
    pub agent_argv: Option<Vec<String>>,
    /// argv for the navigator pane: `[self_bin, view]`.
    pub view_argv: Vec<String>,
}

/// Build the layout plan. `self_bin` is the absolute path to the co-review
/// binary to invoke inside the panes.
pub fn plan_layout(
    self_bin: &str,
    session_dir: &str,
    worktree: &str,
    label: &str,
    with_agent: bool,
) -> LayoutPlan {
    LayoutPlan {
        label: label.to_string(),
        worktree: worktree.to_string(),
        env: vec![
            (
                crate::paths::SESSION_ENV.to_string(),
                session_dir.to_string(),
            ),
            (crate::paths::BIN_ENV.to_string(), self_bin.to_string()),
        ],
        view_argv: vec![self_bin.to_string(), "view".to_string()],
        agent_argv: with_agent.then(|| vec![self_bin.to_string(), "__launch-agent".to_string()]),
    }
}

/// The PR reference to act on: the CLI argument, or — when launched from Herdr's
/// GitHub-PR link handler — the clicked URL from the plugin invocation context.
fn pr_argument(args: &StartArgs, ctx: Option<&PluginContext>) -> Result<String> {
    if let Some(pr) = &args.pr {
        return Ok(pr.clone());
    }
    if let Some(url) = ctx.and_then(PluginContext::clicked_url) {
        return Ok(url.to_string());
    }
    bail!("no pull request given. Pass one, e.g. `co-review start 123`.")
}

/// The directory to discover the source repository from. Plugin commands run
/// with the plugin root as cwd (which is itself a git checkout — of the wrong
/// repo), so when a plugin invocation context is present, prefer the pane the
/// user was actually in.
fn discovery_dir(ctx: Option<&PluginContext>) -> Result<std::path::PathBuf> {
    if let Some(ctx) = ctx {
        for dir in ctx.cwd_candidates() {
            let p = std::path::PathBuf::from(dir);
            if p.is_dir() {
                return Ok(p);
            }
        }
    }
    std::env::current_dir().context("getting current directory")
}

/// Resolve a PR reference to its session directory, discovering owner/repo from
/// the surrounding git repo when the reference is bare. Used by `end`.
pub fn session_dir_for_pr(pr_arg: &str) -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir().context("getting current directory")?;
    // A bare number needs the repo to resolve owner/repo; a full ref does not.
    let git = Git::discover(&cwd).ok();
    let (owner, repo, number) = match &git {
        Some(g) => resolve_pr_ref(pr_arg, g)?,
        None => {
            let pref = crate::pr::parse(pr_arg)?;
            match (pref.owner, pref.repo) {
                (Some(o), Some(r)) => (o, r, pref.number),
                _ => bail!(
                    "'{pr_arg}' needs an owner/repo (run inside the repo, or pass owner/repo#{})",
                    pref.number
                ),
            }
        }
    };
    crate::paths::session_dir(&crate::model::pr_slug(&owner, &repo, number))
}

/// Resolve owner/repo/number from the CLI reference and the surrounding repo.
fn resolve_pr_ref(pr_arg: &str, git: &Git) -> Result<(String, String, u64)> {
    let pref = crate::pr::parse(pr_arg)?;
    let (owner, repo) = match (pref.owner.clone(), pref.repo.clone()) {
        (Some(o), Some(r)) => (o, r),
        _ => {
            let url = git.remote_url("origin").context(
                "could not determine owner/repo: pass owner/repo#123 or run inside the repo",
            )?;
            crate::pr::parse_github_remote(&url).ok_or_else(|| {
                anyhow!("origin remote '{url}' is not a github.com repo; pass owner/repo#123")
            })?
        }
    };
    Ok((owner, repo, pref.number))
}

pub fn start(args: &StartArgs) -> Result<()> {
    let cfg = Config::load()?;
    let ctx = PluginContext::from_env();
    let cwd = discovery_dir(ctx.as_ref())?;
    let git = Git::discover(&cwd)?;

    let pr_arg = pr_argument(args, ctx.as_ref())?;
    let (owner, repo, number) = resolve_pr_ref(&pr_arg, &git)?;

    // Choose the agent and render the prompt.
    let agent_name = args
        .agent
        .clone()
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = cfg.agent(&agent_name).ok_or_else(|| {
        anyhow!(
            "unknown agent '{agent_name}'; known agents: {}",
            cfg.agents.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;

    let slug = crate::model::pr_slug(&owner, &repo, number);
    let session_dir = crate::paths::session_dir(&slug)?;
    let worktree = crate::paths::worktree_path(&slug)?;
    let protocol_path = session_dir.join("CO_REVIEW.md");

    let prompt_template = resolve_prompt(args, &cfg)?;
    let pr_display = format!("#{number}");
    let prompt = crate::protocol::render_prompt(
        &prompt_template,
        &pr_display,
        &protocol_path.to_string_lossy(),
    );
    // The fully resolved agent argv (with the whole prompt) is private launch
    // state: it goes to `agent-launch.json`, never into a typed pane command.
    let launch_spec = if args.no_agent {
        None
    } else {
        Some(AgentLaunchSpec::new(agent.build_command(&prompt)))
    };

    // Session identity: the exact executable performing this start. A resume
    // re-establishes it, so the executable that resumes becomes the session's
    // binary for the newly opened panes.
    let self_bin = crate::paths::require_self_bin()?;
    let label = format!("co-review {owner}/{repo}#{number}");
    let plan = plan_layout(
        &self_bin,
        &session_dir.to_string_lossy(),
        &worktree.to_string_lossy(),
        &label,
        launch_spec.is_some(),
    );

    if args.dry_run {
        print_dry_run(
            &owner,
            &repo,
            number,
            &agent_name,
            &prompt,
            &plan,
            &session_dir,
            &git,
        );
        return Ok(());
    }

    // ---- Real run from here on ----

    if session_dir.join(crate::store::STATE_FILE).is_file() && !args.resume {
        bail!(
            "a co-review session for {owner}/{repo}#{number} already exists at {}.\n\
             Pass --resume to reuse it, or delete that directory to start fresh.",
            session_dir.display()
        );
    }

    let remote = fetch_remote(&git, &owner, &repo);
    let pr = assemble_pr_info(&git, &owner, &repo, number, &remote)?;
    prepare_worktree(&git, &worktree, &pr, args.resume, &remote)?;

    // Persist the session state and protocol file.
    std::fs::create_dir_all(&session_dir)
        .with_context(|| format!("creating session dir {}", session_dir.display()))?;
    crate::util::atomic_write(&protocol_path, crate::protocol::PROTOCOL_MD.as_bytes())?;

    let store = Store::new(&session_dir);
    let session_meta = SessionMeta {
        id: slug.clone(),
        worktree: worktree.to_string_lossy().into_owned(),
        source_repo: git.root().to_string_lossy().into_owned(),
        created_at_ms: crate::util::now_ms(),
        agent_pane_id: None,
        view_pane_id: None,
        workspace_id: None,
        agent_kind: agent.kind.clone().unwrap_or_else(|| agent_name.clone()),
        prompt: prompt.clone(),
    };
    if store.exists() && args.resume {
        // keep existing findings; refresh PR metadata + prompt
        store.update(|s| {
            s.pr = pr.clone();
            s.session.prompt = prompt.clone();
            Ok(())
        })?;
    } else {
        store.create(&State::new(pr.clone(), session_meta))?;
    }

    // (Re)generate the private launch spec on every start, including --resume:
    // the selected agent, its configured argv, the rendered prompt, and the
    // launching executable may all have changed. A --no-agent resume clears a
    // stale spec instead.
    match &launch_spec {
        Some(spec) => crate::agent_launch::write(&session_dir, spec)?,
        None => crate::agent_launch::remove(&session_dir)?,
    }

    eprintln!(
        "co-review session ready for {owner}/{repo}#{number}\n  worktree: {}\n  session:  {}",
        worktree.display(),
        session_dir.display()
    );

    // Driving Herdr is the one part we can't verify locally, so never let a
    // Herdr hiccup lose the (already-prepared) session: on failure, print the
    // exact commands to open the two panes by hand. We still exit non-zero so a
    // script can tell the split didn't open, while the session stays on disk.
    if let Err(e) = launch_layout(&plan, &store) {
        print_manual_fallback(&plan, &e);
        bail!("the Herdr split could not be created automatically (see instructions above); the session is ready to open by hand");
    }
    Ok(())
}

/// When automatic Herdr layout fails, tell the user how to open the panes.
/// This exercises exactly the same contract as the live panes: identity via
/// `$CO_REVIEW_SESSION`/`$CO_REVIEW_BIN`, operations as bare subcommands.
fn print_manual_fallback(plan: &LayoutPlan, err: &anyhow::Error) {
    eprintln!("\nwarning: couldn't set up the Herdr split automatically ({err}).");
    for line in fallback_lines(plan) {
        eprintln!("{line}");
    }
}

/// The manual-open instructions. Every value is shell-quoted: the default
/// macOS session path contains spaces ("Library/Application Support").
fn fallback_lines(plan: &LayoutPlan) -> Vec<String> {
    let mut lines = vec![
        "The worktree and session are ready — open two panes in the worktree yourself:".to_string(),
        format!("  cd {}", shell_quote(&plan.worktree)),
    ];
    for (k, v) in &plan.env {
        lines.push(format!("  export {k}={}", shell_quote(v)));
    }
    lines.push("  navigator: \"$CO_REVIEW_BIN\" view".to_string());
    lines.push(match &plan.agent_argv {
        Some(_) => "  agent:     \"$CO_REVIEW_BIN\" __launch-agent".to_string(),
        None => "  agent:     (start your agent in the other pane)".to_string(),
    });
    lines
}

fn resolve_prompt(args: &StartArgs, cfg: &Config) -> Result<String> {
    if let Some(p) = &args.prompt {
        return Ok(p.clone());
    }
    if let Some(file) = &args.prompt_file {
        return crate::util::read_path_or_stdin(file);
    }
    Ok(cfg.prompt.clone())
}

/// The remote to fetch PR refs from. Normally `origin`; only when origin is
/// recognizably a *different* GitHub repo (e.g. a link handler clicked in a
/// checkout of another repository) fetch from the PR's GitHub URL directly.
/// A non-GitHub origin (local bare repo, self-hosted mirror) stays `origin`.
fn fetch_remote(git: &Git, owner: &str, repo: &str) -> String {
    let differs = git
        .remote_url("origin")
        .ok()
        .and_then(|u| crate::pr::parse_github_remote(&u))
        .is_some_and(|(o, r)| !(o.eq_ignore_ascii_case(owner) && r.eq_ignore_ascii_case(repo)));
    if differs {
        format!("https://github.com/{owner}/{repo}.git")
    } else {
        "origin".to_string()
    }
}

/// Fetch PR metadata from GitHub if a token is available, else fall back to what
/// we can learn from git alone.
fn assemble_pr_info(
    git: &Git,
    owner: &str,
    repo: &str,
    number: u64,
    remote: &str,
) -> Result<PrInfo> {
    if let Some(token) = crate::github::resolve_token() {
        match crate::github::Client::new(token).fetch_pr(owner, repo, number) {
            Ok(pr) => return Ok(pr),
            Err(e) => eprintln!("warning: GitHub API lookup failed ({e}); continuing from git"),
        }
    } else {
        eprintln!("note: no GitHub token; base/head metadata will be inferred from git");
    }
    // Fallback: fetch the PR head, resolve its sha, approximate the base as the
    // merge-base with the origin default branch.
    git.fetch(remote, &git::pr_head_refspec(number))?;
    let head_sha = git.rev_parse(&git::pr_head_ref(number))?;
    let base_sha = default_base_sha(git, &head_sha).unwrap_or_default();
    Ok(PrInfo {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number,
        title: String::new(),
        author: String::new(),
        base_ref: String::new(),
        head_ref: git::pr_head_ref(number),
        base_sha,
        head_sha,
        url: crate::github::pr_html_url(owner, repo, number),
    })
}

/// Best-effort base sha: merge-base of the head and the origin's default branch.
fn default_base_sha(git: &Git, head_sha: &str) -> Option<String> {
    for branch in ["origin/HEAD", "origin/main", "origin/master"] {
        if let Ok(mb) = git.merge_base(head_sha, branch) {
            if !mb.is_empty() {
                return Some(mb);
            }
        }
    }
    None
}

fn prepare_worktree(
    git: &Git,
    worktree: &Path,
    pr: &PrInfo,
    resume: bool,
    remote: &str,
) -> Result<()> {
    // Make sure the head is present locally.
    git.fetch(remote, &git::pr_head_refspec(pr.number))
        .with_context(|| format!("fetching PR #{} head", pr.number))?;
    // Bring the base branch too, so diffs work.
    if !pr.base_ref.is_empty() {
        git.fetch(remote, &pr.base_ref).ok();
    }

    let checkout_rev = if pr.head_sha.is_empty() {
        git::pr_head_ref(pr.number)
    } else {
        pr.head_sha.clone()
    };

    if git.worktree_exists(worktree) {
        if resume {
            // Reuse the worktree but move it to the (possibly newer) head so the
            // files and line numbers match the metadata we just refreshed.
            return git.checkout_detach_in(worktree, &checkout_rev);
        }
        // Recreate it clean.
        git.remove_worktree(worktree).ok();
    }
    git.add_worktree(worktree, &checkout_rev)?;
    Ok(())
}

fn launch_layout(plan: &LayoutPlan, store: &Store) -> Result<()> {
    let herdr = Herdr::new(false);
    if !herdr.available() {
        bail!(
            "herdr not found on PATH. Install Herdr (https://herdr.dev) or set HERDR_BIN_PATH.\n\
             The worktree and session are ready; you can open panes manually, or re-run with \
             --dry-run to see the intended layout."
        );
    }

    // Persist pane ids as soon as each is created, so a later failure still
    // leaves the workspace recorded — `co-review end --force` can then prune the
    // (possibly half-created) panes rather than orphaning them. `end` requires
    // --force here on purpose: with pane ids recorded we can't tell a
    // half-launched session from a live one, so we don't wipe it silently.
    let ws = herdr.workspace_create(&plan.worktree, &plan.label, &plan.env)?;
    let agent_pane = ws.first_pane.clone();
    store.update(|s| {
        s.session.workspace_id = Some(ws.id.clone());
        s.session.agent_pane_id = Some(agent_pane.clone());
        Ok(())
    })?;

    // Navigator on the right; keep focus on the agent pane.
    let view_pane = herdr.pane_split(&agent_pane, false, Some(&plan.worktree), &plan.env)?;
    store.update(|s| {
        s.session.view_pane_id = Some(view_pane.clone());
        Ok(())
    })?;

    herdr.pane_run(&view_pane, &plan.view_argv)?;
    if let Some(agent_argv) = &plan.agent_argv {
        herdr.pane_run(&agent_pane, agent_argv)?;
    }
    herdr.pane_focus(&agent_pane).ok();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_dry_run(
    owner: &str,
    repo: &str,
    number: u64,
    agent_name: &str,
    prompt: &str,
    plan: &LayoutPlan,
    session_dir: &Path,
    git: &Git,
) {
    println!("# co-review start — dry run (no side effects)\n");
    println!("PR:        {owner}/{repo}#{number}");
    println!("agent:     {agent_name}");
    println!("repo:      {}", git.root().display());
    println!("session:   {}", session_dir.display());
    println!("worktree:  {}", plan.worktree);
    println!("\n## git (would run)");
    println!(
        "git fetch --no-tags origin {}",
        git::pr_head_refspec(number)
    );
    println!(
        "git worktree add --detach --force {} <pr-head>",
        plan.worktree
    );
    println!("\n## environment (would set in both panes)");
    for (k, v) in &plan.env {
        println!("{k}={v}");
    }
    if plan.agent_argv.is_some() {
        println!(
            "\n## would write: {}",
            session_dir.join(crate::agent_launch::FILE_NAME).display()
        );
    }
    println!("\n## herdr (would run)");
    let env_flags: String = plan
        .env
        .iter()
        .map(|(k, v)| format!(" --env {k}={v}"))
        .collect();
    println!(
        "herdr workspace create --cwd {} --label {:?}{env_flags}",
        plan.worktree, plan.label
    );
    println!(
        "herdr pane split <w:p1> --direction right --cwd {} --no-focus{env_flags}",
        plan.worktree
    );
    match &plan.agent_argv {
        Some(a) => println!("herdr pane run <w:p1> {}", shell_join(a)),
        None => println!("(--no-agent: left pane left as a shell)"),
    }
    println!("herdr pane run <w:p2> {}", shell_join(&plan.view_argv));
    println!("\n## prompt handed to the agent (via agent-launch.json, never typed)\n");
    println!("{prompt}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{BIN_ENV, SESSION_ENV};

    const SELF_BIN: &str = "/some/private/herdr/plugin/path/co-review";

    #[test]
    fn env_is_exactly_session_and_bin() {
        let plan = plan_layout(SELF_BIN, "/s/dir", "/w/tree", "lbl", true);
        assert_eq!(
            plan.env,
            vec![
                (SESSION_ENV.to_string(), "/s/dir".to_string()),
                (BIN_ENV.to_string(), SELF_BIN.to_string()),
            ],
            "env must carry exactly the two identity variables, in order"
        );
        assert!(
            !plan.env.iter().any(|(k, _)| k == "PATH"),
            "PATH is never part of the session environment"
        );
    }

    /// The pane transport invariant: even with a huge inherited PATH and a
    /// huge prompt, `pane run` carries exactly `<self_bin> view` and
    /// `<self_bin> __launch-agent` — no session path, no prompt, no PATH.
    #[test]
    fn pane_transport_is_minimal() {
        let prompt = "p".repeat(16 * 1024);
        let session_dir = format!("/s/{}", "d".repeat(1024));
        let plan = plan_layout(SELF_BIN, &session_dir, "/w/tree", "lbl", true);

        assert_eq!(
            plan.view_argv,
            vec![SELF_BIN.to_string(), "view".to_string()]
        );
        assert_eq!(
            plan.agent_argv,
            Some(vec![SELF_BIN.to_string(), "__launch-agent".to_string()])
        );

        for argv in [&plan.view_argv, plan.agent_argv.as_ref().unwrap()] {
            for arg in argv {
                assert!(
                    !arg.contains(&session_dir),
                    "session path leaked into {arg:?}"
                );
                assert!(!arg.contains(&prompt[..64]), "prompt leaked into {arg:?}");
                assert!(!arg.contains("PATH"), "PATH leaked into {arg:?}");
                assert!(
                    !arg.contains("CO_REVIEW_SESSION="),
                    "env prefix leaked into {arg:?}"
                );
            }
            let typed = shell_join(argv);
            assert!(
                typed.len() < 512,
                "pane command should be tiny (<512 bytes, well under herdr#2862's ~1 KiB), got {} bytes: {typed:?}",
                typed.len()
            );
        }

        // The full prompt lives only in the private launch spec.
        let spec = AgentLaunchSpec::new(vec!["claude".to_string(), prompt.clone()]);
        assert_eq!(spec.argv[1].len(), 16 * 1024);
    }

    #[test]
    fn plan_without_agent() {
        let plan = plan_layout(SELF_BIN, "/s", "/w", "l", false);
        assert!(plan.agent_argv.is_none());
        // Navigator identity still comes through the environment.
        assert!(plan.env.iter().any(|(k, _)| k == BIN_ENV));
    }

    /// Resume re-establishes binary identity: the plan — and with it the env
    /// Herdr sets on the new panes — follows whichever executable performs the
    /// (re)start.
    #[test]
    fn plan_tracks_the_exact_self_bin() {
        let other = "/opt/dev-checkout/target/debug/co-review";
        let plan = plan_layout(other, "/s", "/w", "l", true);
        assert_eq!(plan.env[1], (BIN_ENV.to_string(), other.to_string()));
        assert_eq!(plan.view_argv[0], other);
        assert_eq!(plan.agent_argv.as_ref().unwrap()[0], other);
    }

    #[test]
    fn fallback_is_valid_shell_under_spacey_paths() {
        // The default macOS session path contains spaces.
        let plan = plan_layout(
            "/Users/test/My Tools/co-review",
            "/Users/test/Library/Application Support/co review/session",
            "/w/tree",
            "l",
            true,
        );
        let lines = fallback_lines(&plan);
        let text = lines.join("\n");
        assert!(text.contains(
            "export CO_REVIEW_SESSION='/Users/test/Library/Application Support/co review/session'"
        ));
        assert!(text.contains("export CO_REVIEW_BIN='/Users/test/My Tools/co-review'"));
        assert!(text.contains("\"$CO_REVIEW_BIN\" view"));
        assert!(text.contains("\"$CO_REVIEW_BIN\" __launch-agent"));
        // Same contract as the live panes: no unquoted path left unprotected,
        // no PATH or env-prefix machinery anywhere.
        assert!(!text.contains("PATH="));
        for line in &lines {
            if line.starts_with("  export ") {
                let value = line.rsplit('=').next().unwrap();
                assert!(
                    value.starts_with('\'') && value.ends_with('\''),
                    "spacey values must be single-quoted: {line}"
                );
            }
        }
    }
}
