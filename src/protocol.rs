//! The agent-facing contract.
//!
//! [`DEFAULT_PROMPT`] is the opening message handed to the agent when a session
//! starts. [`PROTOCOL_MD`] is the full reference the agent can re-read at any
//! time (`co-review protocol`), and is written into the session directory as
//! `CO_REVIEW.md` so it travels with the checkout.
//!
//! Both are plain text on purpose: any agent that can run shell commands can
//! follow them, which is what makes co-review agent-agnostic.

/// Prefix every triage-done pane message starts with. Tests and the protocol
/// text match on this so the three agent-facing docs stay in step.
pub const TRIAGE_DONE_PREFIX: &str = "[co-review] Triage is done — every finding is decided.";

/// The message the navigator injects into the agent pane once the human has
/// decided every finding *and* picked an overall PR verdict. The prompt and
/// protocol tell the agent to act on it, so keep the three texts in step.
pub fn triage_done_msg(
    verdict: crate::model::OverallVerdict,
    findings_summary: &str,
    follow_up: Option<&str>,
) -> String {
    let mut msg = format!(
        "{TRIAGE_DONE_PREFIX} Overall: {}.\nFindings: {findings_summary}.\n",
        verdict.label()
    );
    match (verdict.gh_event(), verdict.gh_flag()) {
        (Some(event), Some(_flag)) => {
            msg.push_str(&format!(
                "Run \"$CO_REVIEW_BIN\" post — one GitHub review with event {event} \
(inline comments for findings with a location, others in the review body). \
Then \"$CO_REVIEW_BIN\" set-status done."
            ));
        }
        _ => {
            let task = follow_up.unwrap_or("(no extra instruction)");
            msg.push_str(&format!(
                "Do not submit a GitHub review. Do this instead: {task}\n\
Then \"$CO_REVIEW_BIN\" set-status reviewing if you add more findings, or \
\"$CO_REVIEW_BIN\" set-status done if you are finished."
            ));
        }
    }
    msg
}

/// Placeholder in the prompt, replaced with the PR reference (e.g. `#123`).
pub const PR_PLACEHOLDER: &str = "{pr}";
/// Placeholder in the prompt, replaced with the absolute path to `CO_REVIEW.md`.
pub const PROTOCOL_PLACEHOLDER: &str = "{protocol}";

/// The default opening prompt. Substitute [`PR_PLACEHOLDER`] and
/// [`PROTOCOL_PLACEHOLDER`] before handing it to the agent.
pub const DEFAULT_PROMPT: &str = r#"You and I are co-reviewing pull request {pr} together, side by side.

You are in the LEFT pane. In the RIGHT pane I have a navigator where I can see
each of your findings with its surrounding code, mark it validated or dismissed,
and talk to you about it. We drive this review together.

First, the ground rule for every co-review command in this session:

  "$CO_REVIEW_BIN" is set by co-review to the exact executable for this
  session. ALWAYS invoke co-review through "$CO_REVIEW_BIN" — never a bare
  `co-review`, which may resolve to a different installation (or nothing).
  `$CO_REVIEW_BIN` and `$CO_REVIEW_SESSION` are provided by the active
  co-review session. Do not guess their values and do not fall back to bare
  `co-review`.

Your job:

1. Do a thorough, high-signal code review of this PR. If you have a `code-review`
   skill, use it. Focus on correctness bugs first, then real
   reuse/simplification/efficiency issues. Skip noise.

2. Record EACH finding by running `"$CO_REVIEW_BIN" add-finding` instead of
   posting anything to GitHub yet. One command per finding, for example:

     "$CO_REVIEW_BIN" add-finding \
       --title "Off-by-one in page slicing" \
       --severity high --impact blocking --category correctness \
       --location src/paginate.rs:42-48 \
       --body "The end index is inclusive here but exclusive at the call site, so the last row is dropped when the page is full."

   Repeat `--location path:line` (or `path:start-end`) for every relevant spot.
   The finding shows up live in my navigator the moment you run the command.

3. When you have added all findings, record your overall opinion of the PR
   with `"$CO_REVIEW_BIN" recommend <approve|request_changes|comment>`, then
   run `"$CO_REVIEW_BIN" set-status awaiting_review`, tell me you're done, and
   END YOUR TURN — do not run a blocking command or poll. While I triage on
   the right, I may message you here about specific findings; respond
   conversationally and, if we agree a finding should change, update it with
   `"$CO_REVIEW_BIN" verdict <id> ...` or `"$CO_REVIEW_BIN" add-finding` /
   edit as needed.

4. When I have decided every finding, my navigator derives the GitHub review
   event from validated findings (blocking → request_changes, only
   non-blocking → comment, none → approve) and asks me to confirm. It sends
   ONE message into this pane only after that confirm, with the overall
   result and every finding verdict. Do not post before that message. Then:
   if overall is approve, comment, or request_changes, run
   `"$CO_REVIEW_BIN" post` (one GitHub review: inline comments for findings
   with a location, the rest in the review body; dismissed findings are
   omitted), then `"$CO_REVIEW_BIN" set-status done`. If overall is
   follow_up, do the extra task in the message instead of posting.

The full contract, including how to read my decisions back, is in {protocol}
(also available via `"$CO_REVIEW_BIN" protocol`). Read it if anything is unclear.

Start the review now."#;

/// The full protocol reference.
pub const PROTOCOL_MD: &str = r#"# co-review agent protocol

This session is a **co-review**: an AI agent (you) and a human review a pull
request together. Shared state lives in a session directory and is mutated only
through co-review subcommands, which handle locking, ids, and timestamps for
you. `$CO_REVIEW_SESSION` points at that directory; you normally don't need it
because the commands find the session automatically.

## Invoking co-review (read this first)

`$CO_REVIEW_BIN` is set by co-review to the exact executable for this session.
Always invoke co-review through `"$CO_REVIEW_BIN"` — never a bare `co-review`,
which may resolve to a different installation (or nothing). `$CO_REVIEW_BIN`
and `$CO_REVIEW_SESSION` are provided by the active co-review session; do not
guess their values and do not fall back to bare `co-review`.

## The loop

1. **Review.** Produce high-signal findings. Correctness bugs first.
2. **Record.** One `"$CO_REVIEW_BIN" add-finding` per finding (see below).
   Findings appear live in the human's navigator.
3. **Hand off.** `"$CO_REVIEW_BIN" recommend <approve|request_changes|comment>`
   with your overall opinion of the PR, then `"$CO_REVIEW_BIN" set-status
   awaiting_review`, tell the human you are done, and end your turn. The
   command prints whether findings are still pending or everything is already
   decided (then wait for the overall verdict message).
4. **Collaborate.** While the human triages, they may message you. Adjust
   findings if you both agree.
5. **Act on the result.** The navigator messages you once, after every
   finding is decided *and* the human confirms the derived GitHub event. That
   message includes the overall verdict and a summary of every finding. Until
   it arrives, do not post. Then:
   - `approve` / `comment` / `request_changes`: `"$CO_REVIEW_BIN" post` (one
     GitHub review: inline comments + body + event), then
     `"$CO_REVIEW_BIN" set-status done`.
   - `follow_up`: do the extra task in the message. Do not post.

## Recording a finding

    "$CO_REVIEW_BIN" add-finding \
      --title "<short title>" \
      --severity <critical|high|medium|low|nit> \
      --impact <blocking|non_blocking> \
      --category <free text, e.g. correctness|security|simplification|efficiency> \
      --location <path:line | path:start-end>   (repeatable) \
      --body "<markdown explanation, ideally with the fix>" \
      [--suggestion "<concrete replacement code>"]

- `--location` may be repeated for multiple spots in one finding.
- Add `@base` to a location (e.g. `src/x.rs:10@base`) to point at the base
  version instead of the PR's head; the default is the head.
- Long markdown: use `--body-file <path>` or `--body-file -` to read stdin.
- `--impact blocking` means: if the human validates this finding, the GitHub
  review event is request_changes. Default is `non_blocking`.
- Bulk: `"$CO_REVIEW_BIN" import <file.json>` ingests a JSON array of findings
  using the same field names as the state schema (`title`, `severity`,
  `impact`, `category`, `body`, `suggestion`, `locations: [{file, start_line,
  end_line, side}]`).

`add-finding` prints the new finding id (e.g. `f3`).

## Reading the human's decisions

- `"$CO_REVIEW_BIN" list --json` prints the full state, including each
  finding's `verdict` (`pending`, `validated`, `dismissed`, `edited`),
  `impact`, and any `user_note` the human attached.
- You do not need to poll for the hand-off: once every finding is decided and
  the human confirms the derived event, the navigator sends one message
  `[co-review] Triage is done … Overall: … Findings: …` into your pane.
- `"$CO_REVIEW_BIN" wait` blocks until every finding has a verdict other than
  `pending` (add `--timeout <ms>` to bound it). It is a fallback for setups
  where the navigator cannot message you (no Herdr, scripted runs); in a normal
  session, end your turn instead — a blocking `wait` shows you as busy while
  you are only waiting.
- `"$CO_REVIEW_BIN" post` submits one GitHub review. Validated and edited
  findings with a location become inline comments on that review. Findings
  without a location go in the review body. Never include `dismissed`
  findings. Chat is not a verdict; a finding stays `pending` until the human
  validates or dismisses it.

## Updating and posting

- `"$CO_REVIEW_BIN" verdict <id> <verdict> [--note "..."]` — set a
  verdict/note (you normally only do this if you and the human agree to change
  one).
- `"$CO_REVIEW_BIN" edit <id> [--title ...] [--severity ...] [--body ...]
  [--body-file -] [--suggestion ...] [--location path:line ...]` — revise an
  existing finding after you and the human discuss it. Only the fields you
  pass change (use `--clear-suggestion` / `--clear-category` /
  `--clear-locations` to remove one). Editing a decided finding resets its
  verdict to `pending` so the revised text gets re-triaged; pass
  `--keep-verdict` to override.
- `"$CO_REVIEW_BIN" post` — submit the one GitHub review from current state
  (`pr_verdict` plus postable findings). Marks those findings posted.
- `"$CO_REVIEW_BIN" mark-posted <id> --url <comment-url>` — record a post if
  you did not use `"$CO_REVIEW_BIN" post`.
- `"$CO_REVIEW_BIN" set-status <reviewing|awaiting_review|posting|done>` —
  move the lifecycle along; the human's navigator shows this status.
- `"$CO_REVIEW_BIN" recommend <approve|request_changes|comment>` — record
  your overall opinion of the PR (`agent_pr_verdict`). Do this before
  hand-off. The GitHub event is derived from validated findings; the human
  confirms it.

Keep it collaborative: the human sees everything you record in real time.
"#;

/// Render the default prompt with the PR reference and protocol path filled in.
pub fn render_prompt(template: &str, pr_display: &str, protocol_path: &str) -> String {
    template
        .replace(PR_PLACEHOLDER, pr_display)
        .replace(PROTOCOL_PLACEHOLDER, protocol_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triage_done_msg_names_the_github_event() {
        let msg = triage_done_msg(
            crate::model::OverallVerdict::RequestChanges,
            "f1 validated — bug",
            None,
        );
        assert!(msg.starts_with(TRIAGE_DONE_PREFIX));
        assert!(msg.contains("Overall: request_changes"));
        assert!(msg.contains("Findings: f1 validated — bug"));
        assert!(msg.contains("REQUEST_CHANGES"));
        assert!(msg.contains("\"$CO_REVIEW_BIN\" post"));
        assert!(msg.contains("\"$CO_REVIEW_BIN\" set-status done"));
    }

    #[test]
    fn triage_done_msg_follow_up_skips_github_review() {
        let msg = triage_done_msg(
            crate::model::OverallVerdict::FollowUp,
            "f1 dismissed — nit",
            Some("re-check the lock"),
        );
        assert!(msg.contains("Overall: follow_up"));
        assert!(msg.contains("Do not submit a GitHub review"));
        assert!(msg.contains("re-check the lock"));
        assert!(!msg.contains("gh pr review"));
    }

    #[test]
    fn render_substitutes_placeholders() {
        let out = render_prompt(DEFAULT_PROMPT, "#123", "/tmp/s/CO_REVIEW.md");
        assert!(out.contains("#123"));
        assert!(out.contains("/tmp/s/CO_REVIEW.md"));
        assert!(!out.contains(PR_PLACEHOLDER));
        assert!(!out.contains(PROTOCOL_PLACEHOLDER));
    }

    #[test]
    fn protocol_mentions_key_commands() {
        for cmd in [
            "add-finding",
            "set-status",
            "wait",
            "post",
            "mark-posted",
            "import",
            "recommend",
        ] {
            assert!(PROTOCOL_MD.contains(cmd), "protocol should mention {cmd}");
        }
    }

    #[test]
    fn protocol_uses_co_review_bin_for_agent_commands() {
        let rendered_prompt = render_prompt(DEFAULT_PROMPT, "#1", "/tmp/s/CO_REVIEW.md");
        for text in [PROTOCOL_MD, &rendered_prompt] {
            // Collapse line wrapping so phrases are matched as prose.
            let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            // No bare `co-review <verb>` — agent-facing instructions must go
            // through "$CO_REVIEW_BIN".
            for verb in [
                "add-finding",
                "set-status",
                "wait",
                "mark-posted",
                "verdict",
                "edit",
                "import",
                "list",
                "status",
                "show",
                "post",
                "protocol",
                "recommend",
            ] {
                assert!(
                    !flat.contains(&format!("co-review {verb}")),
                    "agent-facing text must not contain bare `co-review {verb}`"
                );
            }
            assert!(
                flat.contains("\"$CO_REVIEW_BIN\" add-finding"),
                "agent commands must be invoked through $CO_REVIEW_BIN"
            );
            assert!(
                flat.contains("$CO_REVIEW_SESSION"),
                "the session variables must be named"
            );
            assert!(
                flat.contains("do not fall back to bare `co-review`")
                    || flat.contains("never a bare `co-review`"),
                "the bare-binary warning must be present"
            );
        }
    }

    #[test]
    fn skill_matches_the_protocol_contract() {
        // The skill is agent-facing too; keep it in step with PROTOCOL_MD.
        let skill = include_str!("../skills/co-review/SKILL.md");
        assert!(
            !skill.contains("co-review add-finding"),
            "SKILL.md must use \"$CO_REVIEW_BIN\", not bare co-review"
        );
        assert!(skill.contains("\"$CO_REVIEW_BIN\" add-finding"));
        assert!(skill.contains("\"$CO_REVIEW_BIN\" recommend"));
        assert!(skill.contains("CO_REVIEW_SESSION"));
    }

    #[test]
    fn readme_shows_the_default_prompt() {
        let readme = include_str!("../README.md");
        assert!(
            readme.contains(DEFAULT_PROMPT),
            "README.md must include DEFAULT_PROMPT verbatim"
        );
    }
}
