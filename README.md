# dbnget

A command-line downloader for [Databento](https://databento.com) historical market
data, built on the official `databento` Rust client.

## Install

```sh
brokkr install
```

The API key is read from `DATABENTO_API_KEY`, or passed with `--key`.

## Commands

### `get` - stream a range to DBN files

```sh
dbnget get -d GLBX.MDP3 -s trades -S ESM4,NQM4 --start 2024-05-01 --end 2024-05-08 -o data
```

A bare `--start` date with no `--end` covers exactly that one UTC day.

`--split` issues one request, and writes one file, per trading session. Each file is
downloaded to a `.partial` sidecar and renamed only once complete, so re-running the
same command after an interruption resumes at the first missing session. `--force`
re-downloads sessions that are already present.

Files are named `DATASET.SCHEMA.START-END.dbn.zst`, e.g.
`GLBX_MDP3.trades.20240430T2200-20240501T2100.dbn.zst`. Both bounds are in the name
because a session is not a calendar day.

#### Sessions are not UTC days

Databento reads a date-only bound as UTC midnight. For a venue whose trading day starts
somewhere else that is the wrong boundary: a CME session runs from 17:00 the previous
day to 16:00 America/Chicago, so splitting on UTC midnight clips the front of each
opening session and pulls in the same slice of the session after it.

`--session` picks the convention, and defaults to `auto`:

| Value | Trading day |
|---|---|
| `auto` | derived from the dataset code |
| `cme` | 17:00 previous day to 16:00 America/Chicago, DST-aware |
| `utc` | the UTC calendar day |

`auto` maps `GLBX*` to `cme` and everything else to `utc`. Under `cme` the 16:00-17:00
maintenance break is not part of any session and is never requested, so consecutive
chunks do not tile the calendar.

#### Skipping empty sessions

`--skip-empty` asks `metadata.get_record_count` whether each chunk holds anything, and
skips it if not. That endpoint is unbilled, so weekends, holidays and closures are
skipped without a holiday calendar and without spending anything. It costs one metadata
round-trip per chunk.

Note that `metadata.get_dataset_condition` cannot be used for this: it reports dataset
ingestion status, and answers `available` for Christmas Day and for every Saturday.

### `cost` - price a query before running it

```sh
dbnget cost -d GLBX.MDP3 -s mbo -S ESM4 --start 2024-05-01 --end 2024-06-01
```

Prints the record count, billable size and USD cost.

A quote of $0.00 is ambiguous. Under an active subscription a covered request prices at
zero because it is already paid for, but a request whose symbols match nothing prices at
zero too. The record count disambiguates, and an empty query says so explicitly.

### `batch` - submit and collect batch jobs

```sh
dbnget batch submit -d GLBX.MDP3 -s trades -S ALL_SYMBOLS \
    --start 2024-05-01 --end 2024-06-01 --split-duration month \
    --confirm --max-dollars 50 --wait-and-download data/

dbnget batch list --state queued,processing
dbnget batch status JOB_ID
dbnget batch download JOB_ID -o data/ --wait
```

`--wait` and `--wait-and-download` poll every `--poll-interval` seconds until the job
reaches `done`, and fail if it expires first.

#### Submitting costs money, so it is gated

`batch submit` prices the request and stops. It submits nothing unless given both
`--confirm` and `--max-dollars`, and refuses if the live quote exceeds the cap rather
than truncating the request to fit. A quote that is not a finite number is refused, as
is a request that matches no records.

#### Downloaded files are verified

The client checks batch checksums itself, but on a mismatch it logs a warning and
returns success, and it skips a file whose size already matches without re-reading it.
`dbnget` re-checks size and SHA-256 against the job manifest after every download and
fails if either disagrees, so a corrupt file cannot be mistaken for a complete one by
whatever runs next. Files ending in `.zst` are also checked for a zstd frame header,
which catches the case where the delivered bytes are intact but are not the compressed
data they claim to be. Manifest filenames are rejected unless they are plain file names,
since they are joined onto the output directory.

Files land in `OUT/JOB_ID/`.

### `meta` - dataset metadata

```sh
dbnget meta datasets
dbnget meta schemas GLBX.MDP3
dbnget meta range GLBX.MDP3
dbnget meta publishers
```

## Development

```sh
brokkr check   # gremlins + clippy + tests
brokkr fmt
```
