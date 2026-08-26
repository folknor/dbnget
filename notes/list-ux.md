# `dbnget list` at scale, and the output redesign

Transient. This is a proposal, not a contract. Nothing durable should cite it.

## The forcing problem

An account accrues 472 batch jobs in two weeks on ONE dataset. That is ~34 jobs/day.
Five datasets is ~170/day.

Everything depends on one fact nobody has measured yet:

**Do expired jobs stay in the vendor's job listing, or are they purged?**

- **Persist:** 5 years x 5 datasets is roughly **310,000 jobs**. A per-job detail
  fan-out is not slow, it is impossible. Nothing below saves it.
- **Purged at ~30 days:** steady state is roughly **5,000 jobs** and stays there no
  matter how old the account gets, because the window is retention, not history.

Even 5,000 is fatal for the current shape: `dbnget list` and the pre-submit sweep both
fetch details for every job, one request each.

Note what the API section below establishes: the SHORT listing is one request whatever
the count, so this is a fan-out problem and not a listing problem. `dbnget list` is
therefore fixable with a limit alone. The sweep is not, because "no job matches" is a
claim about a real subset of the account rather than a display choice, and no `--limit`
is legitimate there.

This cannot be measured on the development account yet - it is two weeks old, all 472
jobs are `Done`, and nothing has aged out. `--state expired` returns nothing. MEASURE
THIS FIRST. It decides whether the work below is a redesign or a rewrite.

## What the API actually offers

From the vendor documentation for `batch.list_jobs`, confirmed against the client
source. Two endpoints on `https://hist.databento.com/v0/`, HTTP Basic with the key as
username:

- `GET batch.list_jobs` - parameters are `states`, `since` and `short`, and THAT IS ALL.
  No dataset, schema or symbol filter. No `until`. **No limit and no pagination.**
  Results are sorted by `ts_received`. `states` defaults to all except `expired`.
- `GET batch.get_job_details?job_id=...` - one call per job, returns the full record.

Three consequences, and the third is the important one:

1. Regex selectors can only ever be CLIENT-side. They cannot reduce what the vendor
   sends, only which jobs get a detail fetch - which is exactly why resolving them
   against the request index matters.
2. `--limit` is a local truncation. It cannot save a round trip on the listing, only on
   the fan-out.
3. **The short listing is ONE request at any account size.** At 310,000 jobs that is a
   single response of roughly 30MB - large, not fatal. So the scaling problem was never
   the listing. It is only ever the detail fan-out, and that is entirely ours to control.

That reframes the expiry question below. It decides how big one JSON response gets, not
whether the tool works.

## The architectural half

### 1. The request index becomes the query layer

The index already holds dataset, schema, symbols, bounds and symbology for every job it
has seen. That is exactly what a filter needs. So filters resolve against the index
locally, and live details are fetched only for the surviving subset.

This is a display and selection decision, never a spend decision, which is the line the
index is allowed to be on.

The bootstrap problem stands: on a cold index nothing is known, so a first run still has
to fetch everything. That is what the bounded default below is for.

### 2. The default view is bounded, and the full scan is opt-in

Every tool that lists an unbounded collection does this. `gh pr list` defaults to 30
items. `docker ps` hides stopped containers until `-a`. `kubectl get` is namespaced
until `--all-namespaces`.

Proposal: **the default lists only jobs whose files still exist** - queued, processing,
done - because an expired job has nothing to download and cannot be adopted. That is a
defensible default on its own terms AND it bounds the listing to the retention window
regardless of account age. `--state expired` or `--all` opts into history.

Add `--limit N` as a hard cap and `--since` as it already exists.

### 3. The pre-submit sweep stops being exhaustive over everything

This is the part that needs a spar, because it relaxes a rule that exists to prevent
double charging.

Today: submitting requires live details for EVERY job in the listing. At 5,000 jobs that
is ten minutes before every purchase. At 310,000 it never finishes.

Proposal: the sweep fetches details for

- every job id **absent from the index** - these are jobs dbnget has never seen, which
  is exactly the web-UI-submitted case the exhaustive sweep was protecting against; plus
- every job the index proposes as a candidate, for live confirmation.

Skipped: jobs the index knows and says do not match. On a warm index that reduces a
sweep to the handful of jobs created since the last run.

The residual risk is an index entry that wrongly says "does not match" for a job that
does. Sources, and why each is acceptable:

- Corruption - fails to deserialize, treated as a miss, so the job is fetched anyway.
- Vendor changing the normalization of an existing field - this breaks `job_matches`
  against LIVE data too, so the re-purchase happens with or without the index. The index
  adds no risk here.
- Deliberate tampering with a user-private file on the user's own machine.

What this gives up: the guarantee currently encoded in `NoLiveMatch`. It would become
"exhaustive over jobs dbnget has not already identified" rather than "exhaustive over
the account". That is a weaker claim and the type and its documentation must say so
plainly rather than quietly meaning something new.

## The output half

### The immediate defect

A `parent:` selection of 63 symbols is rendered in full, which destroys the table. Two
rows from a real listing:

```
GLBX-...  Done  GLBX.MDP3  ohlcv-1d    raw_symbol:ESM4    2024-05-01..2024-05-02  ...
GLBX-...  Done  GLBX.MDP3  mbo         parent:ES.FUT,MES.FUT,NQ.FUT,[60 more],DC.FUT  ...
```

The first is good. The second is unreadable and takes the columns after it with it.

### `--format`, and what the formats are for

Precedent: `kubectl -o`, `aws --output`, `docker --format`, `gh --json`.

| Format | For |
|---|---|
| `table` | Default. Terminal-width aware, truncating, paged. |
| `json` | One array. Machine consumption, full fidelity, no truncation ever. |
| `ndjson` | One object per line. The `jq` and streaming case. |
| `csv` | Spreadsheets, and the large part of the data world that still speaks it. |
| `markdown` | Pasting into an issue or a doc. |
| `ids` | Bare job ids, one per line, for `xargs -n1 dbnget get`. |

CSV is RFC 4180: a header row, and any field containing a comma, quote or newline is
quoted with inner quotes doubled. The symbols list is one comma-joined field, which
means it is always quoted - which is exactly why hand-rolling the quoting rather than
eyeballing it matters. No dependency needed; the rule is about fifteen lines.

`ids` matters more than it looks: it is what makes `list` composable with the rest of
the tool, the way `docker ps -q` and `gh pr list --json number` are.

### Table truncation

Only the table format truncates, and it says so. The symbols cell becomes the symbology,
as many symbols as fit, and a count of the rest:

```
parent:ES.FUT,MES.FUT,NQ.FUT +60 more
```

`--wide` disables truncation, following `kubectl -o wide` and `docker --no-trunc`. JSON
and NDJSON are never truncated - a machine format that silently drops data is a trap.

Terminal width comes from the ioctl, falling back to 80 when stdout is not a terminal.
The existing progress meter already established the terminal-detection pattern here.

### Paging

Pipe to `$PAGER` when stdout is a terminal and the output exceeds a screen. `--no-pager`
to defeat it. Precedent: `git`, `systemctl`.

Note the interaction with the progress meter: the meter is on stderr and the table on
stdout, so a pager does not fight it, but the meter must be finished and cleared BEFORE
the pager takes the terminal.

### Regex selectors

`--dataset RE`, `--schema RE`, `--symbol RE`, `--job-id RE`.

- Unanchored regex, case-insensitive. Symbols canonicalize to uppercase, and making
  users type `ES\.FUT` in caps to match their own data would be hostile.
- Repeatable. Same flag repeated is OR, different flags are AND. This is `docker
  --filter` semantics and it is what people expect.
- `--symbol` matches if ANY symbol in the job matches, since a job holds a set.

Resolved against the index where possible, so filtering does not require fetching.

The alternative considered and rejected: a single `--query` expression language, as
`aws --query` does with JMESPath. It is more powerful and much worse to type, and the
four fields above cover what anyone actually asks of a job list.

## Order of work

1. ~~Table truncation.~~ DONE. The cause was not a missing feature: the summariser
   existed and never fired, because the vendor echoes a multi-symbol selection as one
   comma-joined string and the display path never split it.
2. ~~`--format` with `json`, `ndjson`, `csv`, `markdown`, `ids`.~~ DONE.
3. ~~`--limit`.~~ DONE, and it is what makes `list` scale: 4 requests instead of 473 on
   a 472-job account.
4. `--wide` to defeat table truncation. Not yet done.
5. ~~Regex selectors.~~ DONE, but NOT resolved against the index as this note proposed.
   A review killed that: `RequestKey` is safe when under-specified only because it is
   used for positive lookup, and this is the diagnostic surface, so a hint must not be
   what hides a row. Selection is exact and the index stays write-only in `list`. The
   same review found a real bug in the pipeline sketched here - capping rows before
   filtering lets non-matching rows consume the limit - so selection now walks
   newest-first and stops at N MATCHES.
6. Paging.
7. The bounded DEFAULT - still undecided between state-based and time-based, and less
   urgent now that `--limit` exists.
8. **Measure the expiry question.** Demoted: since the short listing is one request at
   any size, this decides how large one response gets, not whether the tool works.
9. The sweep relaxation - LAST, and only after a spar. The only item here that touches
   whether money can be spent twice, and the only one `--limit` cannot help with.
