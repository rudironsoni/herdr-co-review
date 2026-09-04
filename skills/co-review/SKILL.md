---
name: co-review
description: >-
  Drive an interactive, split-screen PR co-review with a human using the
  co-review CLI via "$CO_REVIEW_BIN". Use this whenever you are reviewing a
  pull request inside a co-review session (the environment variables
  CO_REVIEW_SESSION and CO_REVIEW_BIN are set, or the opening prompt references
  co-review): record each finding with "$CO_REVIEW_BIN" add-finding instead of
  posting, hand off with "$CO_REVIEW_BIN" set-status awaiting_review and end
  your turn, then post the human-approved findings and mark them posted.
---

# co-review (agent side)

You are the agent half of a **co-review**: you and a human review a pull request
together. You produce findings; the human triages them in a live navigator that
shows each finding with its surrounding code, and can talk to you about any of
them. Shared state is managed by the co-review CLI, which handles locking, ids,
and timestamps — you never edit its files directly.

You are in a co-review session when `CO_REVIEW_SESSION` and `CO_REVIEW_BIN` are
set in your environment (the `co-review start` command sets both in your pane).
`CO_REVIEW_BIN` is the exact executable for this session: **always invoke
co-review through `"$CO_REVIEW_BIN"`, never a bare `co-review`** — that may
resolve to a different installation (or nothing). Both variables are provided
by the active session; do not guess their values and do not fall back to bare
`co-review`. All subcommands then find the session automatically; no
`--session` needed.

## The loop

1. **Review.** Do a thorough, high-signal review of the PR. Prefer your
   `code-review` skill if you have one. Correctness bugs first, then genuine
   reuse / simplification / efficiency issues. Skip noise — every finding costs
   the human attention.

2. **Record each finding** with `"$CO_REVIEW_BIN" add-finding` (one command per
   finding). It appears in the human's navigator immediately:

   ```
   "$CO_REVIEW_BIN" add-finding \
     --title "Off-by-one in page slicing" \
     --severity high --category correctness \
     --location src/paginate.rs:42-48 \
     --body "The end index is inclusive here but the caller treats it as exclusive, so the last row is dropped on full pages."
   ```

   - Repeat `--location path:line` (or `path:start-end`) for every relevant spot.
   - Append `@base` to a location to point at the base version (default: head).
   - Long markdown: `--body-file <file>` or `--body-file -` (stdin).
   - A concrete fix: `--suggestion "<replacement code>"` (posted as a GitHub
     suggestion block).
   - Bulk alternative: write a JSON array and run `"$CO_REVIEW_BIN" import
     findings.json`.

3. **Hand off.** When every finding is recorded, run:

   ```
   "$CO_REVIEW_BIN" set-status awaiting_review
   ```

   Tell the human you're done, then **end your turn**. Do not run
   `"$CO_REVIEW_BIN" wait` or poll in a loop — a blocking command shows you as
   busy while you are only waiting. The navigator messages you when triage is
   done. If the `set-status` output says every finding is already decided, skip
   ahead and post right away.

4. **Collaborate while the human triages.** The human may message you about a
   specific finding (their messages arrive prefixed like `[co-review f3] …`).
   Discuss it. If you both agree a finding should change, update it:
   `"$CO_REVIEW_BIN" verdict f3 dismissed`, or revise its text with
   `"$CO_REVIEW_BIN" edit f3 --body "…"` (only the fields you pass change), or
   add a new finding.

5. **Post.** When the navigator tells you triage is done (a message like
   `[co-review] Triage is done — every finding is decided. …`), read the
   decisions and post:

   ```
   "$CO_REVIEW_BIN" list --json    # inspect each finding's verdict + user_note
   ```

   - Post findings whose `verdict` is `approved` or `edited` as inline PR review
     comments. Respect any `user_note`. For `edited`, incorporate the human's
     note into what you post.
   - **Never** post `dismissed` findings. Resolve `needs_discussion` with the
     human first.
   - After posting each one: `"$CO_REVIEW_BIN" mark-posted f3 --url <comment-url>`.
   - Finally: `"$CO_REVIEW_BIN" set-status done`.

## Reference

- `"$CO_REVIEW_BIN" protocol` — prints the full contract at any time.
- `"$CO_REVIEW_BIN" list` / `"$CO_REVIEW_BIN" show <id>` — inspect findings
  (with related code).
- `"$CO_REVIEW_BIN" status` — one-line summary.

Keep it collaborative: everything you record is visible to the human the moment
you record it, and everything they decide is visible to you the moment they
decide it.
