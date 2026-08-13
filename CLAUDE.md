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
- Update `CHANGELOG.md`. A user-visible change - a flag, an output, a behavior, a
  refusal - gets an entry in the same commit that makes it. Internal refactors that
  change nothing observable do not. Until the first release actually ships, there is
  nothing to record changes against: work folds into the initial release entry rather
  than accumulating an unreleased section.
- Never commit markdown changes alone - bundle them with the related code commit;
  tag along dirty markdown when committing other changes.
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless explicitly asked. Stop after the commit.
- Never rewrite published history without asking. The one time it was needed - a
  leaked key on 2026-08-13 - the working fix was a local `filter-branch` scrub plus
  DELETING AND RECREATING the GitHub repo, because a force-push leaves the old
  commit reachable by SHA.

### Releasing

`cargo publish` is NEVER run without an explicit instruction to publish. "Get ready to
release" means everything up to step 4 and nothing after it. Ask before crossing that
line, every time - a published version cannot be deleted, only yanked, and the version
number is burned either way.

The order is deliberate: everything reversible happens before anything that is not.

1. **Green and clean.** `brokkr check` passes, `brokkr fmt` has run, and `git status`
   is clean. Never release from a dirty tree.
2. **Version and changelog.** Bump `version` in `Cargo.toml`. Move the changelog's
   unreleased entries under the new version with today's date, and add the link
   reference at the bottom. Commit both together.
3. **Inspect the package.** `cargo package --list` - confirm no `databento.key`, no
   `scratch/`, no `.reference/`, no stray key in any file. Then `cargo publish
   --dry-run`, which builds the crate exactly as crates.io will.
4. **Push.** `git push`. The commit must be on the remote before the tag points at it.
5. **Tag.** `git tag -a v<VERSION> -m "dbnget v<VERSION>"` then `git push origin
   v<VERSION>`. Annotated, not lightweight, and `v`-prefixed to match the changelog
   link.
6. **Publish.** `cargo publish`. Irreversible. Only with explicit instruction.
7. **Release notes.** `gh release create v<VERSION> --title "v<VERSION>" --notes
   "<the changelog section for this version>"`. Nothing new in the notes - if it is
   worth saying, it belongs in the changelog first.

If step 6 fails after step 5 succeeded, fix forward: delete and re-push the tag if the
commit has to change, rather than publishing a version whose tag points somewhere else.
