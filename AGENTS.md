# AGENTS.md

## dbnget

A command-line downloader for Databento historical market data, built on the
official `databento` Rust client. One crate, one binary, no optional features.

The user-facing behavior is documented in `README.md` and it is accurate - read it
first. This file covers what the README does not: why the thing is shaped this way,
and what must not be broken.

## Architecture: the command IS the state machine

There is no local ledger, no state file, and no daemon. Each run reconciles the
request against the vendor's own batch-job listing, because that listing is the only
account of what was actually bought. Re-running the same command is the poll loop:

- No matching job: submit one, subject to the spend gate, exit 3.
- A matching job still preparing: report state and progress, exit 3.
- A matching job done: download into `OUT/JOB_ID/`, verify every file, exit 0.

Matching is on the request itself - dataset, schema, symbols as a case-insensitive
deduplicated set, bounds, symbology, and EVERY output-affecting submission field
(encoding, compression, split duration and size, `pretty_px`, `pretty_ts`,
`map_symbols`, `split_symbols`, `delivery`, `limit`). This is what makes a re-run
adopt the existing job instead of buying the same data a second time, so ANY change to
how a request is normalized into a match key is a change to whether users get
double-charged. Bounds normalize to UTC instants for exactly this reason.

Two traps live here. `map_symbols` is an `Option<bool>` on the submission and a
concrete `bool` on the echoed job, with an ENCODING-DEPENDENT default (true for CSV and
JSON, false for DBN), so it must be resolved before comparing or every text-encoding
re-run buys again. And a field ADDED to `SubmitJobParams` upstream that nobody adds to
`job_matches` is a silent double charge - the test fixtures build both structs as
literals precisely so that a new field fails the build.

Exit code 3 is load-bearing, not decoration. A queued job is neither success nor
failure, and giving it its own code is what lets a shell drive a wave of requests
without dbnget tracking them: submit everything, then re-run until nothing exits 3.
Collapsing 3 into 0 or 1 destroys that workflow.

## Settled decisions (do not relitigate without cause)

- **The spend gate defaults to $0.00.** Databento prices a request an active
  subscription already covers at exactly $0.00, so the default posture is "fetch only
  what I have already paid for". `--spend USD` caps a run. A quote above the cap
  REFUSES rather than truncating the request to fit - silently fetching less than
  asked for is worse than stopping.
- **A $0.00 quote is ambiguous and the record count disambiguates it.** It means
  either "covered by subscription" or "matches nothing". A zero-record request is an
  error, not a free pass. Do not simplify this check away. It applies on ADOPTION as
  well as before a submit: an empty job already on the account is refused rather than
  ignored, because ignoring it would fall through and submit a duplicate.
- **dbnget is published on crates.io.** The spend gate protects arbitrary users with
  billable keys, not just this account. Hold it to that standard: never weaken, skip,
  or default-bypass it on the grounds that some particular key cannot be charged.
- **Downloads are re-verified locally.** The client checks batch checksums itself,
  but on a mismatch it logs a warning and returns SUCCESS, and it skips a file whose
  size already matches without re-reading it. dbnget therefore re-checks size and
  SHA-256 against the job manifest after every download and fails on either. A
  corrupt file must not be mistakable for a complete one by whatever runs next.
- **Manifest filenames are untrusted input, and so is the job id.** Both are joined
  onto the output directory, so anything that is not a plain file name is rejected.
  Validation is only worth something if the VALIDATED name is the one written, which
  is why files are downloaded one at a time by name: the client's download-all
  re-fetches the manifest internally and derives its paths from that second response,
  so a changed second response would reintroduce a path that was already checked.
  Destinations are also refused when a symbolic link already sits there, checked
  before the write rather than after.
- **Free space is the kernel's problem.** dbnget does not police disk space. No POSIX
  tool does - `curl`, `wget`, `cp` and `dd` all write until the filesystem returns
  ENOSPC and report the error. A `--min-free-gb` flag existed and was removed: it
  reimplemented a check the kernel already performs, turned a full disk into an exit
  code indistinguishable from a real failure, and shipped enabled by default.
- **`--immediate` never overwrites and never leaves a partial file under the final
  name.** The request is billed before a byte arrives, so truncating an existing
  result destroys data that was paid for; and with no manifest to verify against, a
  half-written file is indistinguishable from a complete one. An existing destination
  is an error, and the stream lands on a `.part` sibling renamed only on success.
  Filenames carry seconds and nanoseconds because minute precision let two
  differently-priced requests collide.
- **Databento does not fan out per symbol.** A multi-symbol batch submit stays ONE
  job, tested in both the API and the web UI. Code and docs must not assume
  per-symbol jobs.
- **The development account's own key has cash usage disabled**, so it can download
  but never charge. That is a fact about one account and says NOTHING about the spend
  gate, which exists for published users with billable keys. Do not reason from the
  former to the latter.
- **Streaming (`--immediate`) is the secondary path.** DBN only - the streaming API
  delivers no CSV or JSON - and it bills the moment the request is issued. It has no
  session splitting, resume, or empty-day pre-pass; that machinery existed when
  streaming was primary, and with batch as the default the vendor does the splitting.

### Known gaps (documented, not bugs to "fix" silently)

- The window between the submit POST being charged and the job appearing in the
  listing. A process that dies inside it can submit twice on re-run.
- Overlapping ranges. A widened `--end` is a new job that re-purchases the old range;
  dbnget does not warn. `dbnget list` before submitting is the mitigation.
- Vendor file expiry, about 30 days after completion. An expired job is not
  adoptable, so its command quotes full price again - dbnget warns that proceeding is
  a RE-PURCHASE rather than hiding it behind a generic refusal.

## Layout

Detail lives in the code; this is the map.

- `main.rs` - entry point, `Outcome` to `ExitCode` mapping, tracing setup, client
  construction, and API-key resolution (a `db-` value is the key itself, anything
  else is a path to a file holding one; `DATABENTO_API_KEY` gets identical
  treatment).
- `cli.rs` - the clap surface. Symbols are positional; `list` and `dataset` are the
  only subcommands, and a bare invocation is the fetch verb.
- `query.rs` - parsing flat CLI arguments into the client's types: dates, RFC 3339
  instants, the half-open range (a bare `--start` covers exactly that UTC day),
  symbols, states, formats.
- `fetch.rs` - the fetch verb and the reconcile-submit-poll-download state machine.
- `jobs.rs` - the batch-job listing, job matching, and `dbnget list`.
- `dataset.rs` - `dbnget list datasets` and `dbnget dataset` (range, schemas,
  `--publishers`, filtered to the one dataset).
- `spend.rs` - quoting and the spend gate.
- `verify.rs` - post-download size + SHA-256 verification against the manifest, the
  zstd frame-header check, and manifest filename validation.

## Rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - all verboten.
- Don't remind the user of the rules. They wrote them.
- The user can exempt you from any rule at any time.
- Never write a real API key into any file that is not `databento.key`, and never
  into documentation, tests, commit messages, or example output. Use an obviously
  fake placeholder. A real key in `DESIGN.md` cost this project its key on
  2026-08-13: GitHub secret scanning notified the vendor and it was auto-revoked
  within about thirty seconds of the push.
- `databento.key` is gitignored and must stay that way. `Cargo.toml` also carries an
  `exclude` guard so it can never enter a published package.
- Release-enforce vs `debug_assert!`: if the invariant bounds SPEND or data
  integrity, or a release path can actually hit it, enforce it in release - a
  `bail!`, clamp, or other check that survives `--release` - because brokkr builds
  release and `debug_assert!` compiles out there. Pair the `debug_assert!` with that
  release enforcement so tests still fail loudly. Reserve a bare `debug_assert!` for
  invariants whose failure cannot affect a release run.
- The lint list in `Cargo.toml` is deliberately strict (`unwrap_used`,
  `cast_possible_truncation`, `too_many_lines`, `cognitive_complexity`, and friends
  are all `deny`). Fix the code, don't loosen the lint. `#[expect(...)]` with a
  `reason` is acceptable in tests.

## Commands

Use `brokkr` (not `cargo`) for check/test. Output is filtered to changed files and
capped at 20 diagnostics per phase by default.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope)
- `brokkr check --all` - every diagnostic, no cap, no scope filter
- `brokkr fmt` - run before every commit
- `brokkr install` - installs the `dbnget` binary
- `brokkr test <NAME>` - release-mode focused single-test runner; `<NAME>` is a
  case-sensitive substring filter. `--debug` builds dev profile instead.
- `brokkr run [ARGS]...` - thin wrapper over `cargo run`; forwards arguments raw.

Single crate with no optional features, so `brokkr.toml` declares one `default`
check sweep. ADDING A NON-DEFAULT FEATURE TO `Cargo.toml` MEANS ADDING A `[[check]]`
FOR IT, otherwise it is silently unchecked.

## Testing against the live API

Tests that would hit Databento cost money and need a key. Prefer unit tests over
pure functions - parsing, matching, verification, formatting - which is where nearly
all the logic that can be wrong actually lives. Never write a test that submits a
batch job.

## Document folders

The standing layout, across every project. Three live folders plus one retired,
split by durability first, subject second.

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | Durable in-repo reference for anyone working on or with the code - how the thing is built and why: architecture, invariants, protocol contracts, the durable record of measured numbers over time | Citable from source as a source of truth. What it says must be true. |
| `docs/` | Durable in-repo documentation of how the thing is used - guides, CLI reference, the consumer-facing API surface | Same must-be-true rule. |
| `notes/` | Transient - work items (`todo.md`), future plans, hypotheticals, bug reports, research, analysis. Things that will die | No truth guarantee. Nothing durable cites it. |
| `plans/` | Retired | Plan documents are transient: they go in `notes/`. |

`reference/` and `docs/` are both durable and both binding. The difference is
subject, not audience: `reference/` covers how the thing is built and why - what you
need in order to change it safely - while `docs/` covers how it is used. A developer
or consumer reads both. `notes/` is neither durable nor binding, which is the whole
point of keeping it separate: a document that may be wrong must not sit where a
document that must be right is expected.

The dependency direction is therefore one-way. `notes/` may cite `docs/` and
`reference/`; nothing durable may cite `notes/` - not a code comment, not `docs/`,
not `reference/`. A code comment must carry its full context, because it outlives
the note.

dbnget is small enough that none of these folders exist yet; `README.md` carries the
usage documentation. Create them when there is something durable to put in them, not
before.

**Root-level convention files are exempt.** `AGENTS.md`, `CLAUDE.md`, `README.md`,
`LICENSE`, `CHANGELOG.md` and their kin are found by tooling and by convention at
the repository root, and stay there.

In `notes/`, `docs/` and `reference/` alike, avoid citing source line numbers - they
drift fast.
