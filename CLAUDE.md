@AGENTS.md

## More rules

### Subagents

**Always get permission** from the user before launching subagents.

**Do NOT use git worktree isolation for parallel agents.** Worktrees create merge
conflicts that silently drop agent work. Instead, launch agents in the same tree with
strict file ownership - zero overlap.

Agent coordination rules:
- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file FIRST. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr` or `cargo`. The orchestrator validates between agents.

Audit protocol:
- Do not trust agent claims of completion. Verify existence + wiring + behavior.
- Any discrepancies doc should contain only current gaps, not historical records. Remove resolved items entirely.

Subagent prompt rules:
- Scope the investigation, not the report. Caps like "under 1500 chars" or "max 15 findings" throw away signal you asked them to surface.
- Invite lateral findings up front. If they notice a bug, optimization, smell, or anything surprising while doing the scoped work, they should flag it, even when it's outside the immediate task.
- Name the question, not the method. Don't prescribe tools ("use `git diff`", "use `Read`"), don't prescribe steps, don't enumerate files when the scope already implies them. Prescribing the method wastes tokens and signals distrust.
- Don't restate rules the agent already inherits. Subagents load the same CLAUDE.md / AGENTS.md as the main session, so the bash rules, no-cargo, no-worktrees, gremlins, etc. are already in scope. Re-listing them is noise.
- Do pass anything learned in *this* conversation that the agent can't see: the user's framing, prior decisions, what's already been ruled out, the specific claim being audited.

### Communication rules

- Never use the `AskUserQuestion` tool - the harness runs in don't-ask mode and it
  will be denied. When you need a decision from the user, just ask in chat with the
  options laid out in prose.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not
suggest saving things to memory. Durable context belongs in AGENTS.md, CLAUDE.md, or
the relevant docs, where it is reviewable and versioned with the code.

### Bash rules

- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never `find /`.
- Never run `git` with `-C <path>`
- One Bash() invocation === one command
- Keep `git commit -m` messages free of zsh metacharacters - braces `{}`, brackets
  `[]`, parens `()`, angle brackets `<>`, `#`. They trip the permission matcher and
  block the commit. Spell lists out (`fetch, jobs and verify`, not
  `{fetch,jobs,verify}`), write `5.1 per file` not `5.1/file`, name attributes in
  prose not `#[attr]`.
- Quote `gh api` arguments containing `[]` - zsh globs them and the call fails
  before it is sent.

### git commit rules

- Never run `git checkout --`, `git restore`, or `git stash` against working-tree
  changes. Other agents' uncommitted work may be sitting in those files, and it is
  unrecoverable once discarded - an agent has already destroyed a whole round of
  work this way. Discarding a dirty file is an operation only the user may ask for
  explicitly.
- Always run `brokkr fmt` before a commit.
- Never commit markdown changes alone - bundle them with the related code commit;
  tag along dirty markdown when committing other changes.
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless explicitly asked. Stop after the commit.
- Never rewrite published history without asking. The one time it was needed - a
  leaked key on 2026-08-13 - the working fix was a local `filter-branch` scrub plus
  DELETING AND RECREATING the GitHub repo, because a force-push leaves the old
  commit reachable by SHA.

### Publishing

- `cargo publish` is NEVER run without an explicit instruction to publish. "Get
  ready to publish" means metadata and packaging only. Verify with
  `cargo package --list` that no key, no `scratch/`, and no `.reference/` is in the
  tarball.
