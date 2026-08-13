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

`--daily` issues one request per UTC day and writes one file per day. Because each
file is downloaded to a `.partial` sidecar and renamed only once complete, re-running
the same command after an interruption resumes at the first missing day. `--force`
re-downloads days that are already present.

Files are named `DATASET.SCHEMA.DATE.dbn.zst`, e.g. `GLBX_MDP3.trades.20240501.dbn.zst`.

### `cost` - price a query before running it

```sh
dbnget cost -d GLBX.MDP3 -s mbo -S ESM4 --start 2024-05-01 --end 2024-06-01
```

Prints the record count, billable size and USD cost.

### `batch` - submit and collect batch jobs

```sh
dbnget batch submit -d GLBX.MDP3 -s trades -S ALL_SYMBOLS \
    --start 2024-05-01 --end 2024-06-01 --split-duration month \
    --wait-and-download data/

dbnget batch list --state queued,processing
dbnget batch status JOB_ID
dbnget batch download JOB_ID -o data/ --wait
```

`--wait` and `--wait-and-download` poll every `--poll-interval` seconds until the job
reaches `done`, and fail if it expires first.

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
