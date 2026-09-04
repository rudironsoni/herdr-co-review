//! End-to-end integration tests of the agent-facing flow against a local bare
//! repository standing in for GitHub. They drive the real compiled binary.
//!
//! Skips gracefully if `git` is unavailable. No network: the GitHub token is
//! forced empty so `start` takes the pure-git fallback path, and
//! `CO_REVIEW_FAKE_HERDR=1` makes the Herdr layer print instead of execute.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_co-review")
}

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run the co-review binary against a session, returning (stdout, stderr, ok).
fn co_review(
    home: &Path,
    session: Option<&Path>,
    cwd: &Path,
    args: &[&str],
) -> (String, String, bool) {
    co_review_env(home, session, cwd, args, &[])
}

/// Like [`co_review`], with extra environment variables set.
fn co_review_env(
    home: &Path,
    session: Option<&Path>,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (String, String, bool) {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("CO_REVIEW_HOME", home)
        .env("CO_REVIEW_FAKE_HERDR", "1")
        // Force the no-token path so we never touch the network.
        .env("GH_TOKEN", "")
        .env("GITHUB_TOKEN", "");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Explicitly control session identity: unset unless a session is given.
    cmd.env_remove("CO_REVIEW_SESSION");
    if let Some(s) = session {
        cmd.env("CO_REVIEW_SESSION", s);
    }
    let out = cmd.output().expect("spawn co-review");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A fresh clone of an empty bare "origin", with git identity configured.
/// Returns `(co_review_home, work_repo)`.
fn new_clone(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let bare = root.join("origin.git");
    let work = root.join("work");
    git(root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(
        root,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    (home, work)
}

/// A standard PR: `main` holds the base file, and a feature branch (pushed to
/// `refs/pull/1/head`) changes line 2 and adds line 4.
fn make_pr_repo(root: &Path) -> (PathBuf, PathBuf) {
    let (home, work) = new_clone(root);
    std::fs::write(work.join("f.txt"), "a\nb\nc\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("f.txt"), "a\nB2\nc\nd\n").unwrap();
    git(&work, &["commit", "-qam", "change"]);
    git(&work, &["push", "-q", "origin", "HEAD:refs/pull/1/head"]);
    (home, work)
}

#[test]
fn full_agent_flow() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let session = home.join("sessions").join("owner-repo-1");
    let wt = home.join("worktrees").join("owner-repo-1");

    // start
    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");
    assert!(
        session.join("state.json").is_file(),
        "state.json not created"
    );
    assert!(
        session.join("CO_REVIEW.md").is_file(),
        "protocol file not written"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("f.txt")).unwrap(),
        "a\nB2\nc\nd\n"
    );

    // add-finding -> prints id f1
    let (out, err, ok) = co_review(
        &home,
        Some(&session),
        &work,
        &[
            "add-finding",
            "--title",
            "Check line 2",
            "--severity",
            "high",
            "--location",
            "f.txt:2",
            "--body",
            "line 2 changed",
        ],
    );
    assert!(ok, "add-finding failed: {err}");
    assert_eq!(out.trim(), "f1");

    // list --json -> one pending finding
    let (out, _e, ok) = co_review(&home, Some(&session), &work, &["list", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 1);
    assert_eq!(v["findings"][0]["verdict"], "pending");
    assert_eq!(v["findings"][0]["severity"], "high");

    // edit revises only the passed fields
    let (_o, err, ok) = co_review(
        &home,
        Some(&session),
        &work,
        &[
            "edit",
            "f1",
            "--title",
            "Off-by-one on line 2",
            "--severity",
            "critical",
        ],
    );
    assert!(ok, "edit failed: {err}");
    let (out, _e, _ok) = co_review(&home, Some(&session), &work, &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["findings"][0]["title"], "Off-by-one on line 2");
    assert_eq!(v["findings"][0]["severity"], "critical");
    assert_eq!(v["findings"][0]["body"], "line 2 changed"); // untouched

    // hand off while f1 is still pending → the agent is told to end its turn
    let (out, _e, ok) = co_review(
        &home,
        Some(&session),
        &work,
        &["set-status", "awaiting_review"],
    );
    assert!(ok);
    assert!(
        out.contains("1 finding(s) pending") && out.contains("end your turn"),
        "expected the pending hand-off hint: {out}"
    );

    // deciding the last finding tells the agent triage is done
    let (out, err, ok) = co_review(&home, Some(&session), &work, &["verdict", "f1", "approved"]);
    assert!(ok, "verdict failed: {err}");
    assert!(
        out.contains("all findings decided"),
        "expected the triage-done hint: {out}"
    );

    // a hand-off after triage reports there is nothing left to wait for
    let (out, _e, ok) = co_review(
        &home,
        Some(&session),
        &work,
        &["set-status", "awaiting_review"],
    );
    assert!(ok);
    assert!(
        out.contains("already decided"),
        "expected the already-decided hint: {out}"
    );

    // the `wait` fallback returns immediately (0 pending)
    let (_o, err, ok) = co_review(&home, Some(&session), &work, &["wait", "--timeout", "3000"]);
    assert!(ok, "wait did not return: {err}");

    // post --dry-run lists the approved finding without needing a token
    let (out, err, ok) = co_review(&home, Some(&session), &work, &["post", "--dry-run"]);
    assert!(ok, "post --dry-run failed: {err}");
    assert!(out.contains("would post 1 finding"), "unexpected: {out}");
    assert!(out.contains("Off-by-one on line 2")); // the edited title

    // sessions lists the live session
    let (out, _e, ok) = co_review(&home, None, &work, &["sessions"]);
    assert!(ok);
    assert!(
        out.contains("owner/repo #1"),
        "sessions missing entry: {out}"
    );

    // end removes the worktree and the session directory (--force: panes still
    // recorded from the fake-herdr start)
    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
    assert!(
        !session.join("state.json").exists(),
        "session dir not removed"
    );
    assert!(!wt.join(".git").exists(), "worktree not removed");
}

#[test]
fn resume_updates_worktree_to_new_head() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = new_clone(root.path());
    std::fs::write(work.join("f.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("f.txt"), "one\ntwo\n").unwrap();
    git(&work, &["commit", "-qam", "v1"]);
    git(&work, &["push", "-q", "origin", "HEAD:refs/pull/1/head"]);

    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");
    let wt = home.join("worktrees").join("owner-repo-1");
    assert_eq!(
        std::fs::read_to_string(wt.join("f.txt")).unwrap(),
        "one\ntwo\n"
    );

    // PR is rebased/amended so its head no longer descends from what we fetched
    // (a non-fast-forward update — the case the `+` in the refspec handles).
    std::fs::write(work.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&work, &["commit", "-qam", "v1 amended", "--amend"]);
    git(
        &work,
        &["push", "-q", "-f", "origin", "HEAD:refs/pull/1/head"],
    );

    // resume should move the worktree to the new head.
    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1", "--resume"]);
    assert!(ok, "resume failed: {err}");
    assert_eq!(
        std::fs::read_to_string(wt.join("f.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
}

#[test]
fn edit_resets_decided_verdict_and_clears_fields() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let session = home.join("sessions").join("owner-repo-1");

    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");

    co_review(
        &home,
        Some(&session),
        &work,
        &[
            "add-finding",
            "--title",
            "T",
            "--severity",
            "low",
            "--location",
            "f.txt:2",
            "--suggestion",
            "let x = 1;",
            "--category",
            "style",
        ],
    );
    co_review(&home, Some(&session), &work, &["verdict", "f1", "approved"]);

    // editing a decided finding resets its verdict and can clear fields
    let (out, err, ok) = co_review(
        &home,
        Some(&session),
        &work,
        &[
            "edit",
            "f1",
            "--body",
            "new body",
            "--clear-suggestion",
            "--clear-category",
        ],
    );
    assert!(ok, "edit failed: {err}");
    assert!(
        out.contains("reset to pending"),
        "expected reset note: {out}"
    );

    let (out, _e, _ok) = co_review(&home, Some(&session), &work, &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["findings"][0]["verdict"], "pending");
    assert_eq!(v["findings"][0]["body"], "new body");
    assert!(
        v["findings"][0]["suggestion"].is_null(),
        "suggestion not cleared"
    );
    assert!(
        v["findings"][0]["category"].is_null(),
        "category not cleared"
    );
}

/// Launched via Herdr's link handler, `start` gets no CLI argument and runs
/// with the plugin root as cwd: the PR URL and the repo directory must both
/// come from $HERDR_PLUGIN_CONTEXT_JSON.
#[test]
fn plugin_context_supplies_url_and_repo_dir() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    // Deliberately run from a directory that is not the repo (like a plugin
    // action would).
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let ctx = serde_json::json!({
        "clicked_url": "https://github.com/owner/repo/pull/1",
        "focused_pane_cwd": work.to_str().unwrap(),
        "workspace_cwd": null,
    })
    .to_string();

    let (_o, err, ok) = co_review_env(
        &home,
        None,
        &elsewhere,
        &["start"],
        &[("HERDR_PLUGIN_CONTEXT_JSON", &ctx)],
    );
    assert!(ok, "start from plugin context failed: {err}");
    let session = home.join("sessions").join("owner-repo-1");
    assert!(session.join("state.json").is_file(), "session not created");
    let wt = home.join("worktrees").join("owner-repo-1");
    assert_eq!(
        std::fs::read_to_string(wt.join("f.txt")).unwrap(),
        "a\nB2\nc\nd\n"
    );

    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
}

#[test]
fn doctor_runs_without_a_session() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let (out, _e, ok) = co_review(&home, None, root.path(), &["doctor"]);
    assert!(ok, "doctor should succeed");
    assert!(out.contains("co-review"));
}

#[test]
fn completions_generate_for_each_shell() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let (out, err, ok) = co_review(&home, None, root.path(), &["completions", shell]);
        assert!(ok, "completions {shell} failed: {err}");
        assert!(!out.trim().is_empty(), "completions {shell} were empty");
    }

    let (out, err, ok) = co_review(&home, None, root.path(), &["man"]);
    assert!(ok, "man failed: {err}");
    assert!(
        out.contains(".TH co-review"),
        "man page missing roff header"
    );
}

#[test]
fn start_writes_launch_spec_and_identity_env() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let session = home.join("sessions").join("owner-repo-1");

    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");

    // The private launch spec carries argv only — with the full prompt inside
    // — and none of PATH/CO_REVIEW_SESSION/CO_REVIEW_BIN.
    let spec_path = session.join("agent-launch.json");
    assert!(spec_path.is_file(), "agent-launch.json not written");
    let spec = std::fs::read_to_string(&spec_path).unwrap();
    assert!(spec.contains("\"argv\""));
    assert!(
        spec.contains("co-reviewing pull request"),
        "the full prompt must live in the spec"
    );
    // argv only: no PATH transport, no structured identity fields (the serde
    // shape is enforced by unit tests; the prompt may *mention* variables).
    assert!(!spec.contains("PATH="), "PATH must not appear in the spec");
    assert!(!spec.contains("\"session\""));
    assert!(!spec.contains("\"cwd\""));

    // Identity travels through Herdr's --env on create AND split; PATH never
    // appears in anything co-review drives.
    let bin_flag = format!("--env CO_REVIEW_BIN={}", bin());
    assert_eq!(
        err.matches(&bin_flag).count(),
        2,
        "CO_REVIEW_BIN must be set on workspace create and pane split:\n{err}"
    );
    assert!(err.contains("--env CO_REVIEW_SESSION="));
    assert!(!err.contains("PATH="), "no PATH transport:\n{err}");

    // pane run carries only the operation: short, no session path, no prompt,
    // no --session argument.
    let pane_runs: Vec<&str> = err.lines().filter(|l| l.contains("pane run")).collect();
    assert_eq!(pane_runs.len(), 2, "expected two pane run lines:\n{err}");
    for line in &pane_runs {
        assert!(
            !line.contains("--session"),
            "no --session in pane run: {line}"
        );
        assert!(
            !line.contains(session.to_str().unwrap()),
            "session path must not be typed: {line}"
        );
        assert!(
            !line.contains("co-reviewing pull request"),
            "prompt must not be typed: {line}"
        );
        assert!(line.len() < 512, "pane run must stay tiny: {line}");
    }
    assert!(
        pane_runs.iter().any(|l| l.contains("__launch-agent")),
        "agent pane runs __launch-agent:\n{err}"
    );

    // end removes the private launch state with the session.
    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
    assert!(
        !spec_path.exists(),
        "agent-launch.json must be gone after end"
    );
    assert!(!session.exists());
}

/// PATH size/composition is irrelevant to session orchestration now: a huge,
/// duplicate-ridden PATH changes nothing (git still needs a working PATH, so
/// the noise is appended, not substituted).
#[test]
fn start_is_independent_of_path() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());

    let real = std::env::var("PATH").unwrap_or_default();
    let noise: Vec<std::ffi::OsString> = (0..300)
        .map(|i| format!("/noise/dir{i}-{i}").into())
        .collect();
    let mut entries = std::env::split_paths(&real).map(|p| p.into_os_string());
    let all: Vec<std::ffi::OsString> = entries
        .by_ref()
        .chain(noise.iter().cloned())
        .chain(noise.iter().cloned())
        .collect();
    let fat = std::env::join_paths(all).unwrap();

    let (_o, err, ok) = co_review_env(
        &home,
        None,
        &work,
        &["start", "owner/repo#1"],
        &[("PATH", fat.to_str().unwrap())],
    );
    assert!(ok, "start failed with a huge PATH: {err}");
    assert!(!err.contains("/noise/dir0-0"), "PATH leaked:\n{err}");
    assert!(!err.contains("PATH="), "no PATH transport:\n{err}");

    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
}

/// `__launch-agent` resolves the session strictly from $CO_REVIEW_SESSION:
/// missing, empty, or wrong means fail closed — never the sole session on disk.
#[test]
fn launch_agent_fails_closed_without_session_env() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");

    // Exactly one session exists on disk — it must NOT be picked up.
    let (_o, err, ok) = co_review(&home, None, &work, &["__launch-agent"]);
    assert!(!ok, "launch without CO_REVIEW_SESSION must fail");
    assert!(
        err.contains("CO_REVIEW_SESSION"),
        "error must name it: {err}"
    );

    let wrong = root.path().join("nope");
    let (_o, err, ok) = co_review(&home, Some(&wrong), &work, &["__launch-agent"]);
    assert!(!ok, "launch with a wrong CO_REVIEW_SESSION must fail");
    assert!(
        err.contains("CO_REVIEW_SESSION"),
        "error must name it: {err}"
    );

    // The hidden command takes no --session argument; one must not be
    // silently ignored.
    let (_o, err, ok) = co_review(
        &home,
        None,
        &work,
        &["__launch-agent", "--session", "/whatever"],
    );
    assert!(!ok, "--session must be an unknown argument here");
    assert!(
        err.contains("--session") || err.contains("unexpected"),
        "the error must point at the argument: {err}"
    );

    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
}

/// The hidden command must not leak into help, completions, or the man page.
#[test]
fn launch_agent_is_hidden_from_generated_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    for args in [
        &["--help"][..],
        &["completions", "zsh"][..],
        &["completions", "bash"][..],
        &["man"][..],
    ] {
        let (out, err, ok) = co_review(&home, None, root.path(), args);
        assert!(ok, "{args:?} failed: {err}");
        assert!(
            !out.contains("__launch-agent"),
            "{args:?} must not expose __launch-agent"
        );
    }
}

/// End-to-end launch: the launcher runs the session's resolved argv directly,
/// with no shell. The configured "agent" is this very binary running
/// `add-finding --title <prompt>` — a real mutation that only works when the
/// strictly resolved $CO_REVIEW_SESSION reaches the launched process.
#[test]
fn launch_agent_execs_the_resolved_argv() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let session = home.join("sessions").join("owner-repo-1");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_agent = \"selftest\"\n[agents.selftest]\ncommand = [{:?}, \"add-finding\", \"--title\", \"{{prompt}}\"]\n",
            bin()
        ),
    )
    .unwrap();

    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");

    let (out, err, ok) = co_review(&home, Some(&session), &work, &["__launch-agent"]);
    assert!(ok, "launch failed: {err}");
    assert_eq!(out.trim(), "f1", "add-finding prints the new id");

    // The finding's title is the entire default prompt, byte-for-byte —
    // quotes, unicode, and newlines survive because argv never sees a shell.
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session.join("state.json")).unwrap())
            .unwrap();
    let title = state["findings"][0]["title"].as_str().unwrap();
    assert!(
        title.starts_with("You and I are co-reviewing pull request #1 together"),
        "prompt must reach the agent argv verbatim: {title:.80}..."
    );
    assert_eq!(state["findings"][0]["id"], "f1");

    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
}

/// A resume rewrites the launch spec: the selected prompt/agent may have
/// changed, and the spec must follow.
#[test]
fn resume_regenerates_the_launch_spec() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());
    let session = home.join("sessions").join("owner-repo-1");

    let (_o, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1"]);
    assert!(ok, "start failed: {err}");
    let before = std::fs::read_to_string(session.join("agent-launch.json")).unwrap();
    assert!(!before.contains("CUSTOM-PROMPT-XYZ"));

    let (_o, err, ok) = co_review(
        &home,
        None,
        &work,
        &[
            "start",
            "owner/repo#1",
            "--resume",
            "--prompt",
            "CUSTOM-PROMPT-XYZ {pr} {protocol}",
        ],
    );
    assert!(ok, "resume failed: {err}");
    let after = std::fs::read_to_string(session.join("agent-launch.json")).unwrap();
    assert!(after.contains("CUSTOM-PROMPT-XYZ"));
    assert_ne!(before, after);

    let (_o, err, ok) = co_review(&home, None, &work, &["end", "owner/repo#1", "--force"]);
    assert!(ok, "end failed: {err}");
}

/// --dry-run stays side-effect free and shows the identity contract: env on
/// both panes, short pane commands, no PATH, no prompt in pane run.
#[test]
fn dry_run_writes_nothing_and_shows_the_contract() {
    if !have_git() {
        eprintln!("git unavailable; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (home, work) = make_pr_repo(root.path());

    let (out, err, ok) = co_review(&home, None, &work, &["start", "owner/repo#1", "--dry-run"]);
    assert!(ok, "dry-run failed: {err}");
    assert!(
        !home.join("sessions").exists(),
        "dry-run must not create the session dir"
    );
    assert!(
        out.contains("agent-launch.json"),
        "must name what it would write"
    );
    assert!(out.contains("--env CO_REVIEW_BIN="));
    assert!(!out.contains("PATH="), "no PATH injection shown:\n{out}");
    for line in out.lines().filter(|l| l.contains("pane run")) {
        assert!(
            !line.contains("--session"),
            "no --session in pane run: {line}"
        );
        assert!(
            !line.contains("co-reviewing pull request"),
            "no prompt in pane run: {line}"
        );
    }
}
