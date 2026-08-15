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

`ALL_SYMBOLS` is the trap with the largest bill attached. The vendor echoes symbols as
a JSON array, and the client maps only a SCALAR `"ALL_SYMBOLS"` string to
`Symbols::All`, so a whole-dataset job comes back as an ordinary one-element list.
Comparing the enum variants directly never matched, and the request it failed to match
is the most expensive one an account can make. Both forms canonicalize to the sentinel.

The job listing is fetched with an EXPLICIT state filter, never an omitted one. An
omitted filter means "all except expired" server-side, which made the expired
re-purchase warning unreachable, and it admits states this client's `JobState` cannot
deserialize - `received`, which a job passes through before it is queued - failing the
entire listing and every command with it. The price of naming the states is that a job
still in `received` is invisible, so the documented submit-to-listing window covers it.

Two more traps live here. `map_symbols` is an `Option<bool>` on the submission and a
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

- **The spend gate defaults to $0.00, and caps LIST PRICE rather than the bill.** The
  cost endpoint prices a request with no reference to the account: data a subscription
  covers quotes identically to data nobody paid for, and it answers in sub-cent
  fractions. Measured 2026-08-15: `XNAS.ITCH ohlcv-1m MSFT` for one day quotes
  `0.000479400158`, while the finished job for that same data reports `cost_usd: 0.0`.
  The earlier claim here that a covered request quotes exactly zero was FALSE against
  the live API. The practical consequence is that the $0.00 default refuses nearly
  everything with records in it, and that is accepted rather than fixed: rounding the
  comparison to cents would make the default reachable by authorizing real sub-cent
  charges under a cap the user set to zero, and nothing available before a submit can
  tell "covered" from "cheap", so a `--free` mode would have nothing to compute.
  `--spend USD` caps a run. A quote above the cap REFUSES rather than truncating the
  request to fit - silently fetching less than asked for is worse than stopping.
- **A printed price never understates, and is always itself an acceptable `--spend`.**
  That single rule is `money`, and `minimum_cap` IS `money` with the dollar sign
  removed - not a parallel rule that agrees with it, because a parallel rule is what
  drifted. Rounding to nearest is the defect: at two decimals a refusal read "quoted
  $0.00 exceeds the --spend cap of $0.00", asserting that zero exceeds zero; fixing
  only the sub-cent branch moved the same contradiction above a cent, where a true
  $0.0105 printed as `$0.01`, was refused by a $0.01 cap that accepted a genuine $0.01,
  and suggested `0.02` - twice the needed cap. So: shortest rendering that reproduces
  the value exactly, else shortest with two significant digits, rounded UP. The
  comparison in `approve` never widens. Test
  `the_printed_price_is_always_an_acceptable_cap` is the invariant; keep it passing.
- **`--cost` answers the gate question, not just the price question.** Anyone running
  it is deciding whether to run the real command; a price read without the verdict says
  "free" at an amount the very next invocation refuses. It respects a `--spend` passed
  alongside. The output path is printed only under `--immediate`, because a batch job
  lands in `OUTPUT/JOB_ID/` and the id does not exist until submit - printing a guess
  would be inventing one.
- **Every fetch run states which path it took.** Batch and streaming differ in billing
  and latency and refuse at the same gate, so without it a refused run leaves the user
  unable to tell whether a job was queued on the account.
- **The vendor crate is held back to `info` until `-vvv`.** Its methods are
  instrumented with the client itself as a span field, so each line at its debug level
  carries the whole `BatchClient` - `Url` struct fields, proxies, headers - per span:
  measured at 9.5 KB for one `--cost` run, against 209 bytes of signal. What that noise
  was being read for, that a job listing was fetched and adoption attempted, dbnget
  logs itself. Do not surface vendor logging earlier in the ladder to "help debug"; log
  the thing being debugged instead.
- **An empty file at an output path is a claim, not data.** Telling someone they
  already paid for a zero-byte husk sends them looking for a completed job to reuse,
  which is a long way from a file they can delete. The two cases get different
  messages, and only a file with bytes in it earns the warning about paying twice.
- **A $0.00 quote is ambiguous and the record count disambiguates it.** A zero-record
  request is an error, not a free pass. Do not simplify this check away. It applies on
  ADOPTION as well as before a submit: an empty job already on the account is refused
  rather than ignored, because ignoring it would fall through and submit a duplicate.
- **`dbnget get JOB_ID` is the escape hatch from exact matching, and it cannot
  charge.** Adoption requires reproducing the request field for field, which is
  correct for deciding whether to spend and useless for retrieving a job whose command
  was lost. Downloading a prepared job costs nothing, so this path has no spend gate;
  it shares the manifest verification with adoption.
- **`dbnget list` must show every field adoption keys on that is not obvious.**
  Rendering bounds as bare dates hid an intraday job's times, so a 12:30-14:00 job
  printed as `2022-06-10..2022-06-10` and read as a whole-day job with an inclusive
  end - filed as an end-exclusive matcher bug when the matcher was right. Encoding and
  `limit` were invisible for the same reason, and so was `stype_out`, which sits in the
  OUTPUT column rather than beside the symbols because the symbol column qualifies the
  selection with the symbology it is WRITTEN in. Ranges carry nanoseconds when they have
  them, since adoption compares nanosecond instants and whole seconds would print
  `12:30:00.1` and `12:30:00.9` identically. A listing that cannot explain a non-match
  sends people looking for bugs in the match key. The listing is still NOT complete:
  `job_matches` also reads `compression`, `split_duration`, `split_size`,
  `split_symbols`, `delivery`, `pretty_px` and `map_symbols`, and none of them appear.
  dbnget submits fixed values for all of them, so a job it created cannot differ - but a
  job created in the web UI can, and it renders identically to an adoptable one while
  refusing to be adopted. Adding a field to `job_matches` without asking how a user
  would SEE that field is how this defect keeps recurring. The same rule covers the two byte
  counts: `actual_size` is uncompressed data and `package_size` is the download, they
  differ by 4-5x on compressed DBN and in the other direction on a handful of small
  text files, so one unlabelled number gets read as the download and is not it.
- **`dbnget dataset CODE` carries unit prices, and `list datasets` stays bare codes.**
  `metadata.list_datasets` returns codes and nothing else - there is no description
  field to print, and a hand-maintained table of vendor blurbs would rot silently.
  `metadata.list_unit_prices` is authoritative and per schema, and it is the fact that
  makes two datasets comparable: `ohlcv-1m` is $12.00 on EQUS.MINI and $35.00 on
  DBEQ.BASIC. Historical mode only; live is not a mode this tool fetches in.
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
- **`--immediate` claims both output names, exclusively, before the request is priced
  or issued.** The request is billed before a byte arrives, so truncating an existing
  result destroys data that was paid for; and with no manifest to verify against, a
  half-written file is indistinguishable from a complete one. Asking whether a file
  exists and then writing it is a race two concurrent runs both win, and treating an
  I/O error as "does not exist" turns a permissions problem into a charge. So both the
  destination and its `.part` sibling are created with an exclusive create up front,
  the final rename only ever replaces this run's own placeholder, and a failed request
  releases the claims so a retry is possible - and so does a refusal by the spend gate,
  which is the case that was missed: a run that charged nothing left two empty files
  behind, and the next run reported the data as already paid for. So does a FAILURE OF
  THE SECOND CLAIM, which strands the first the same way. Every exit between the first
  claim and the first byte must release what it took; nothing after the first byte
  arrives may release anything. THE FILE NAME MUST KEY ON THE WHOLE REQUEST,
  for the same reason the match key does: it carried only dataset, schema and range, so
  AAPL and MSFT over one window shared a path, and the second request was told the
  first's file was data it had already paid for - advice that destroys the first
  instrument's data if followed, never yields the second if ignored, and under a script
  exits nonzero over a file holding the wrong instrument. Symbols, symbology in and out,
  bounds and limit now go through `jobs::canonical_symbols` into a digest appended to
  the name, so the two definitions of "the same request" cannot drift apart. Filenames
  carry seconds and nanoseconds because minute precision let two differently-priced
  requests collide. The symbol segment beside the digest is a READING convenience and is
  sanitized, never validated: a symbol is not obliged to be a legal filename, and
  refusing to fetch `ES:FUT` over how its file would be named would be absurd.
- **A corrupt batch file is removed before the client is asked for it again.** The
  client skips any file whose size already matches the manifest, without reading it, so
  a same-size corrupt file would otherwise fail verification on every retry forever
  with no recovery but deleting it by hand - for data already paid for. An existing
  file that verifies is kept and not re-fetched; one that does not is discarded first.
- **Filename validation is the union of platform rules, not the running platform's.**
  The crate is not declared Unix-only. Colons, the Windows-illegal punctuation, control
  characters, trailing dots and spaces, and reserved device names like `CON` and `NUL`
  are all refused, on top of the separator and parent-directory checks. These names are
  vendor job ids and generated data filenames, so a refusal means something upstream is
  wrong.
- **The spend gate fails closed on any quote it cannot account for.** Non-finite and
  negative both. A negative quote is not a discount, and it would pass every
  non-negative cap including the $0.00 default.
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
- `cli.rs` - the clap surface. Symbols are positional; `list`, `dataset` and `get` are
  the only subcommands, and a bare invocation is the fetch verb.
- `query.rs` - parsing flat CLI arguments into the client's types: dates, RFC 3339
  instants, the half-open range (a bare `--start` covers exactly that UTC day),
  symbols, states, formats.
- `fetch.rs` - the fetch verb and the reconcile-submit-poll-download state machine.
- `jobs.rs` - the batch-job listing, job matching, and `dbnget list`.
- `dataset.rs` - `dbnget list datasets` and `dbnget dataset` (range, schemas, unit
  prices, `--publishers`, filtered to the one dataset).
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
