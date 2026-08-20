# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The vendor client moves to `databento` 0.59 (DBN 0.67) and hashing to `sha2`
  0.11. No dbnget behavior changes: the job-matching fixtures build the
  submission and echoed-job structs as literals, so a new output-affecting
  submission field would have failed the build, and it did not.

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
