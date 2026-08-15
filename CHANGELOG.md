# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `dbnget get JOB_ID` downloads a finished batch job by id, with the same manifest
  verification adoption uses. Reconciliation only adopts a job when the command
  reproduces the original request exactly, which left jobs whose command was lost with
  no way to retrieve data the account had already bought.
- `dbnget list` shows each job's encoding and record limit. Both are part of what a
  request must match to adopt a job, and neither was visible.
- `dbnget list` gains a header row and a `DOWNLOAD` column. The size shown was the
  uncompressed data size, which overstates a compressed DBN download by 4-5x; the
  delivered package size now sits beside it, and both are labelled.
- `dbnget dataset CODE` shows per-schema unit prices. Two datasets carrying the same
  symbol can differ several-fold - `ohlcv-1m` is $12.00 per unit on EQUS.MINI and
  $35.00 on DBEQ.BASIC - and nothing in the tool hinted at it.
- `--cost` reports whether the spend gate would let the request through, naming the
  smallest `--spend` value that would be accepted. With `--immediate` it also prints
  the path it would write.
- Every fetch run states whether it is streaming or reconciling against the account's
  batch jobs. The two differ in billing and latency and nothing said which you got.

### Fixed

- Prices below a cent are printed at the precision they need. `--spend 0` against a
  request quoted at $0.000479 refused with "quoted $0.00 exceeds the --spend cap of
  $0.00", which asserts that zero exceeds zero and gives no usable cap. It now reads
  `quoted $0.00048`. The comparison is unchanged and still exact.
- `dbnget list` shows a job's times when its bounds are not midnight-aligned. An
  intraday job printed as `2022-06-10..2022-06-10` was indistinguishable from a
  whole-day job with an inclusive end.
- `--immediate` releases both output claims when the spend gate refuses the run.
  A refusal charged nothing but left an empty destination and `.part` file behind, so
  the next attempt failed with "already exists ... rather than paying for it again"
  about data that was never bought.

- Spend refusals name the smallest `--spend` value that would be accepted, rounded up
  so the suggestion actually works, and mention that a matching job already on the
  account is adopted rather than re-bought. Adoption was previously documented only
  inside the help for the positional symbols argument.
- `--immediate` output filenames key on the whole request, not just dataset, schema and
  range. Two symbols fetched over one window previously shared a path, and the second
  request was told the first's file was data it had already paid for: following that
  advice discards the first instrument's data, ignoring it never yields the second, and
  under a script it exits nonzero over a file holding the wrong instrument. Names now
  carry a symbol segment and a digest of the fields adoption matches on - symbols,
  symbology in and out, bounds and limit - so different requests get different files
  and two spellings of one request still get the same file. **Existing `--immediate`
  files keep their old names; nothing reads them back, but a re-run writes to the new
  name rather than recognising the old one.**
- `--immediate` distinguishes an abandoned zero-byte claim from a file holding data.
  The old message told users they had already paid for what was an empty husk left by
  a refused run.

### Changed

- `-vv` no longer enables the vendor client's logging; that moves to `-vvv`. Its spans
  carry the entire client struct on every line, which came to 9.5 KB for a single
  `--cost` run against 209 bytes of signal. The reconcile steps it was being read for
  are now logged by dbnget directly.
- `--spend`'s documentation no longer claims the default fetches what a subscription
  covers. The cost endpoint prices requests with no reference to the account, so
  covered data still quotes a list price and the $0.00 default refuses nearly
  everything with records in it. The gate is unchanged; the description was wrong.

## [0.1.0] - 2026-08-13

Initial release.

[0.1.0]: https://github.com/folknor/dbnget/releases/tag/v0.1.0
