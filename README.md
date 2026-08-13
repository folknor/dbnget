# dbnget

A command-line downloader for [Databento](https://databento.com) historical market
data, built on the official `databento` Rust client.

## Install

```sh
brokkr install
```

### The API key

`--key` takes either the key itself or a path:

- A value starting with `db-` is the API key.
- Anything else is read as a path to a file holding the key and nothing else;
  surrounding whitespace is trimmed.
- Absent, it falls back to `DATABENTO_API_KEY`, which gets the same treatment.

## The single verb

```sh
dbnget ESM4 NQM4 -d GLBX.MDP3 -s trades --start 2024-05-01 --end 2024-05-08 -o data
```

Symbols are positional. A bare `--start` date with no `--end` covers exactly that one
UTC day. `--format dbn|csv|json` picks the delivered encoding, always zstd-compressed
and split into one file per day.

The command is the state machine. The whole request is one batch job on the vendor's
account (Databento keeps a multi-symbol submit as a single job), and each run
reconciles the request against the account:

- **No matching job**: queue one, subject to the spend gate, and exit 3.
- **A matching job is still preparing**: report its state, how long it has been in
  it, and the vendor's percent completion, and exit 3.
- **A matching job is done**: download it into `OUT/JOB_ID/`, verify every file, and
  exit 0.

Re-running the same command *is* the poll loop. Matching is on the request itself -
dataset, schema, symbols as a case-insensitive set, bounds, symbology, format -
because the vendor's job listing is the only account of what was actually bought.
There is no local ledger; a re-run of a command whose first attempt succeeded adopts
the existing job instead of buying the same data twice. The one gap is the window
between the submit POST being charged and the job appearing in the listing: a process
that dies in it can submit twice on re-run.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Settled. The work is done. |
| 3 | Nonterminal. Nothing is wrong; the data is not ready yet. Run the same command again. |
| 1 | Failed. |

A batch job that is still queued is neither a success nor a failure, so it gets its
own code. That lets a shell drive a wave of requests without dbnget tracking them:
submit everything first (the vendor prepares jobs in parallel), then re-run the same
commands until none exits 3.

### The spend gate: `--spend`

Without `--spend`, any request that would cost more than $0.00 is refused with the
quoted price. Databento prices a request an active subscription already covers at
exactly $0.00, so the default is "fetch only what I have already paid for":

```
$ dbnget ES.FUT --stype-in parent -d GLBX.MDP3 -s tbbo --start 2020-05-04 --end 2020-05-05
Error: this request costs $0.79 (377685 records, 30214800 billable bytes, $0.79);
pass --spend USD to approve the charge
```

`--spend USD` caps what a run may charge. A quote above the cap refuses rather than
truncating the request to fit; a non-finite quote is refused; and a request matching
zero records is an error, not a free pass - a $0.00 quote can mean "covered" or
"matches nothing", and the record count disambiguates.

The vendor deletes a job's prepared files about 30 days after completion. An expired
job is not adoptable, so re-running its command quotes full price like a fresh query
- but dbnget warns that the job expired and that proceeding is a re-purchase, rather
than hiding it behind a generic refusal.

### `--cost`: price it first

```sh
dbnget ES.v.0 --stype-in continuous -d GLBX.MDP3 -s mbp-1 --start 2024-05-01 --end 2024-06-01 --cost
```

Prints the record count, billable size and USD cost, and stops. Never queues.

### `--immediate`: stream it now

One plain streaming request, written straight to disk as
`DATASET.SCHEMA.START-END.dbn.zst`. DBN only - the streaming API does not deliver
CSV or JSON. The spend gate applies exactly as above; streaming bills the moment the
request is issued.

There is no session splitting, resume, or empty-day pre-pass here. That machinery
existed when streaming was the primary path; with batch as the default, the vendor
does the splitting.

### Verified downloads

The client checks batch checksums itself, but on a mismatch it logs a warning and
returns success, and it skips a file whose size already matches without re-reading
it. dbnget re-checks size and SHA-256 against the job manifest after every download
and fails if either disagrees, so a corrupt file cannot be mistaken for a complete
one by whatever runs next. Files ending in `.zst` are also checked for a zstd frame
header, and manifest filenames are rejected unless they are plain file names, since
they are joined onto the output directory.

`--min-free-gb` refuses to start a download when the output filesystem is below the
floor, rather than filling it and taking the rest of the machine down with it.

## `dbnget list`

```sh
dbnget list
dbnget list --state queued,processing
```

Lists the jobs on the account: ID, state, dataset, schema, symbols qualified by their
symbology (`ES.FUT` is one instrument as a raw symbol and every ES future as a parent
symbol), range, cost and size. It is the tool for checking what already exists before
submitting - a widened `--end` is a new job that re-purchases the old range, and
dbnget does not warn about overlaps.

```
GLBX-20260812-MUCPBX5U6K  Done  GLBX.MDP3  tbbo    continuous:ES.v.0   2025-08-12..2025-08-31   $0.00  347.8 MiB
GLBX-20260805-JUBCRPRLG8  Done  GLBX.MDP3  trades  continuous:NQ.v.0,MNQ.v.0  2026-07-05..2026-07-19  $24.06  1.8 GiB
```

Long symbol lists are truncated with a count of what was left out.

## `dbnget meta` - dataset metadata

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
