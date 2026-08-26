# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A request index under the platform cache directory - `~/.cache/dbnget` on Linux,
  honouring `XDG_CACHE_HOME`. It records which request each job on the account was
  submitted with, so a re-run can go straight to the job that already bought the
  data instead of asking the vendor about every job it has.

  It is a hint and never an authority. A job is only ever adopted after the vendor
  confirms it live, and nothing is ever submitted until a complete live sweep of the
  account has found no match, so a stale, corrupt, hostile or absent index cannot
  cause the same data to be bought twice. If the directory cannot be created, made
  private, or read, the index is silently disabled and the command works exactly as
  it would have without it. It holds no credential and no costs or sizes, only which
  request belongs to which job id, and it is written user-private.

  Delete it whenever you like. It rebuilds itself.

- `dbnget list` gains `--format`: `table` (default), `json`, `ndjson`, `csv`,
  `markdown` and `ids`. Only the table abbreviates anything - every other format
  writes the complete symbol list, because a truncation that helps in a column is a
  silent data loss in a file another program parses. CSV is RFC 4180 with a header
  row, so a comma-joined symbol list stays one quoted column rather than becoming
  sixty-three extra ones. `ids` prints bare job ids for `xargs -n1 dbnget get`.
- `dbnget list --limit N` shows only the most recent N jobs, and applies the cap
  before fetching anything. The vendor returns every job in one response and offers
  no limit of its own, so the cost of a listing is one detail request per row - and
  this is where that cost is decided. On an account of 472 jobs, `--limit 3` is four
  requests instead of 473.

### Changed

- The vendor client moves to `databento` 0.60 (DBN 0.68) and hashing to `sha2`
  0.11. The job-matching fixtures build the submission and echoed-job structs as
  literals, so a new output-affecting submission field would have failed the build,
  and it did not.
- 0.60 narrows the vendor's job listing to a short form carrying only a job's id,
  state and received-time; the full details of a job are now a request per job.
  dbnget matches on every field of a request and `dbnget list` shows them, so both
  now fetch those details rather than relying on the deprecated whole-listing call,
  which the vendor will remove.

  The visible cost is that commands which have to look at the whole account got
  slower, and on a long-lived account they got a lot slower - `dbnget list` on an
  account of 472 jobs went from about a second to three minutes. Details are fetched
  four at a time, which brings that back to about eighty seconds, and a vendor rate
  limit is waited out rather than failing the command. Ordinary re-runs of a fetch
  are unaffected: those go through the index and cost a request or two.
- A run that has to read every job on the account now shows a progress counter on
  stderr, so a long wait is distinguishable from a hang - most importantly on the
  pass that happens just before a fetch offers to spend money. It draws only to a
  terminal, leaving piped and redirected output byte-for-byte as it was, and stands
  aside under `-v`, where the same progress goes to the log instead.
- Log output is only coloured when stderr is a terminal. Redirecting the log to a
  file used to write ANSI escape sequences into it.
- `dbnget get JOB_ID` asks about that one job rather than fetching the entire
  account listing to find it.

## [0.2.0] - 2026-08-15

### Added

- `dbnget get JOB_ID` downloads a finished batch job by id, with the same manifest
  verification adoption uses - the way back to paid data when the original command
  is lost.
- `dbnget list` gains a header row and shows everything adoption matches on:
  encoding, output symbology, record limit, intraday times and fractional seconds
  in bounds, and a labelled `DOWNLOAD` (package) size beside the uncompressed data
  size. Jobs whose compression, splitting, delivery or pretty/mapping options
  differ from what dbnget submits are marked.
- `dbnget dataset CODE` shows per-schema unit prices.
- `--cost` reports whether the spend gate would accept the request, naming the
  smallest `--spend` that would; with `--immediate` it also prints the output path.
- Every fetch run states whether it is streaming or reconciling against the
  account's batch jobs.
- Bounds with sub-microsecond precision warn that the vendor may not echo them
  exactly, which would stop a re-run from recognising the job it bought.
- Interrupted downloads resume from where they stopped instead of restarting from
  zero. A right-length file that fails its checksum is still discarded.
- A download holds an exclusive lock on `OUT/JOB_ID/`; a concurrent run reports it
  and exits 3. The lock is released by the OS when the process ends, so it can
  never go stale.

### Fixed

- Jobs whose symbols come back as numeric instrument ids now match the command that
  bought them, instead of being re-purchased on every run. The same fix stops two
  selections that adoption calls different - `42` and `"42"` - from sharing one
  `--immediate` filename.
- `--immediate` no longer warns that streaming is billed above the quote it was gated
  on. Measured against the API, `historical` and `historical-streaming` price a request
  identically, so `--spend` is an exact ceiling on both paths.
- `dbnget get` reports a job's actual state instead of claiming a finished job delivered
  no files. A job still being prepared exits 3 so a poll loop waits for it; an expired
  one is an error naming the 30-day file expiry, since waiting will never produce it.
- `--cost --immediate --format csv` no longer prints a `.dbn.zst` path for a run that
  would be refused for not being DBN.

- Prices print at the shortest precision that reproduces them exactly, rounding up
  otherwise, so the printed figure is never below the real price and is always
  itself an acceptable `--spend`. Nearest-cent rounding could refuse with "quoted
  $0.00 exceeds the --spend cap of $0.00" and suggest caps twice what was needed.
- Spend refusals name the smallest `--spend` that would be accepted, and mention
  that a matching job on the account is adopted rather than re-bought.
- `--immediate` output filenames key on the whole request - symbols, symbology in
  and out, bounds, limit - not just dataset, schema and range, so two different
  requests can no longer collide on one path and be mistaken for already-paid
  data. **Existing files keep their old names; a re-run writes to the new name
  rather than recognising the old one.**
- `--immediate` releases both output claims when the spend gate refuses the run or
  when the second claim fails, instead of stranding empty files that the next run
  reported as data already paid for.
- `--immediate` distinguishes an abandoned zero-byte claim from a file holding
  data, and tells you to delete it rather than describing a payment that never
  happened.

### Changed

- The vendor client's logging moves from `-vv` to `-vvv`; its spans carry the
  whole client struct on every line. The reconcile steps it was being read for are
  now logged by dbnget directly.
- `--spend`'s documentation no longer claims the $0.00 default fetches what a
  subscription covers - the cost endpoint quotes list price regardless of
  coverage, so the default refuses nearly everything with records in it.

## [0.1.0] - 2026-08-13

Initial release.

[0.2.0]: https://github.com/folknor/dbnget/releases/tag/v0.2.0
[0.1.0]: https://github.com/folknor/dbnget/releases/tag/v0.1.0
