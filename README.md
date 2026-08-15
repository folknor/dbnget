# dbnget

[![crates.io](https://img.shields.io/crates/v/dbnget.svg)](https://crates.io/crates/dbnget)
[![downloads](https://img.shields.io/crates/d/dbnget.svg)](https://crates.io/crates/dbnget)
[![license](https://img.shields.io/crates/l/dbnget.svg)](LICENSE)
[![msrv](https://img.shields.io/badge/msrv-1.97-blue.svg)](https://releases.rs)
[![Built with LLMs](https://img.shields.io/badge/Built%20with-AI%20Agents-blueviolet)](LLM.md)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

A command-line downloader for [Databento](https://databento.com) historical market
data, built on the official `databento` Rust client.

Built with LLMs. See [LLM.md](LLM.md). Contributions welcome - see
[CONTRIBUTING.md](CONTRIBUTING.md).

- [Install](#install)
- [Quick start](#quick-start)
- [Exit codes](#exit-codes)
- [The spend gate: `--spend`](#the-spend-gate---spend)
- [`--cost`: price it first](#--cost-price-it-first)
- [`--immediate`: stream it now](#--immediate-stream-it-now)
- [`dbnget list`](#dbnget-list)
- [`dbnget get JOB_ID`](#dbnget-get-job_id---download-a-job-by-name)
- [`dbnget dataset`](#dbnget-dataset---one-datasets-facts)
- [Symbology](#symbology---stype-in-and---stype-out)
- [Verified downloads](#verified-downloads)
- [Verbosity](#verbosity)
- [Development](#development)

## Install

```sh
cargo install dbnget
```

### The API key

`--key` takes either the key itself or a path:

- A value starting with `db-` is the API key.
- Anything else is read as a path to a file holding the key and nothing else;
  surrounding whitespace is trimmed.
- Absent, it falls back to `DATABENTO_API_KEY`, which gets the same treatment.

## Quick start

```sh
dbnget ESM4 NQM4 -d GLBX.MDP3 -s trades --start 2024-05-01 --end 2024-05-08 -o data
```

Symbols are positional. An omitted `--end` means exactly 24 hours after `--start` - for
a plain `YYYY-MM-DD` start that is that one UTC day, and for an RFC 3339 start it is the
24 hours following that instant, not the rest of the calendar day. `--format
dbn|csv|json` picks the delivered encoding, always zstd-compressed and split into one
file per day.

The command is the state machine. The whole request is one batch job on the vendor's
account (Databento keeps a multi-symbol submit as a single job), and each run
reconciles the request against the account:

- **No matching job**: queue one, subject to the spend gate, and exit 3.
- **A matching job is still preparing**: report its state, how long it has been in
  it, and the vendor's percent completion, and exit 3.
- **A matching job is done**: download it into `OUT/JOB_ID/`, verify every file, and
  exit 0.

Re-running the same command *is* the poll loop. Matching is on the request itself -
dataset, schema, symbols as a case-insensitive set with repeats collapsed, bounds,
both symbologies, format, `--limit`, and every remaining field that affects the
delivered bytes - because the vendor's job listing is the only account of what was
actually bought. There is no local ledger; a re-run of a command whose first attempt
succeeded adopts the existing job instead of buying the same data twice.

Anything in that list is part of the request's identity, so changing it makes a
different job rather than the same one spelled differently. `--limit` is the easiest
one to change by accident: adding it to a command that already bought the unlimited
form buys the data again.

The one gap is the window, bounded by seconds, between the submit POST being charged
and the job appearing in the listing: a process that dies in it, or a re-run inside
it, can submit twice.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Settled. The work is done. |
| 3 | Nonterminal. Nothing is wrong; the data is not ready yet. Run the same command again. |
| 1 | Failed. |

A batch job that is still queued is neither a success nor a failure, so it gets its
own code. That lets a shell drive a wave of requests without dbnget tracking them:
submit everything first (the vendor prepares jobs in parallel), then re-run the same
commands until none exits 3.

## The spend gate: `--spend`

Without `--spend`, any request with a price above $0.00 is refused with the quote:

```
$ dbnget ES.FUT --stype-in parent -d GLBX.MDP3 -s tbbo --start 2020-05-04 --end 2020-05-05
Error: this request is priced at $0.79 (377685 records, 30214800 billable bytes, $0.79);
pass --spend USD to approve the charge
```

**What the cap is checked against is the vendor's list price, not your bill.** The
cost endpoint prices a request with no reference to your account: data an active
subscription covers quotes exactly the same as data nobody has paid for, and it
answers in fractions of a cent. A day of one symbol's minute bars is priced at
$0.00048, and the job it produces then shows `$0.00` on the account. So the $0.00
default refuses nearly every request that holds records, and there is deliberately no
"only if it is free" mode - nothing available before a submit can tell "covered" from
"cheap". Price with `--cost`, then pass a cap at or above it.

Prices are printed under one rule: **the figure shown is never lower than the real
price, and is always itself a `--spend` value that would be accepted.** A price is
written at the shortest precision that reproduces it exactly, rounding up where none
does:

```
$ dbnget MSFT -d XNAS.ITCH -s ohlcv-1m --start 2022-06-10 --spend 0
Error: quoted $0.00048 exceeds the --spend cap of $0.00; --spend 0.00048 would approve it
```

`--spend USD` caps what a run may charge. A quote above the cap refuses rather than
truncating the request to fit, and a request matching zero records is an error, not a
free pass - a $0.00 quote can mean "covered" or "matches nothing", and the record
count disambiguates. That holds on adoption too: an empty job already sitting on the
account is refused, not silently re-submitted.

The vendor deletes a job's prepared files about 30 days after completion. An expired
job is not adoptable, so re-running its command quotes full price like a fresh query -
but dbnget warns that the job expired and that proceeding is a re-purchase, rather
than hiding it behind a generic refusal.

## `--cost`: price it first

```sh
dbnget ES.v.0 --stype-in continuous -d GLBX.MDP3 -s mbp-1 --start 2024-05-01 --end 2024-06-01 --cost
```

Prints the record count, billable size, USD cost, whether the spend gate would let the
request through, and how the symbols resolve, then stops. Never queues, and the gate
does not apply - it is a quote, not a purchase. A query matching no records says so.

```
$ dbnget ES.FUT --stype-in parent -d GLBX.MDP3 -s tbbo --start 2022-06-01 --end 2022-06-30 --cost
records:       13011614
billable size: 1040929120 bytes
cost:          $27.14
would fetch:   NO - refused by the default $0.00 cap; pass --spend 27.14 to approve it
symbols:       37 resolved, 5 partial, 0 not found
```

The `would fetch` line is the question `--cost` is usually being asked, so it is
answered outright. It respects a `--spend` passed alongside it, and the cap it names
is the printed price itself - the smallest value that would be accepted.

With `--immediate`, a `would write` line names the exact output path. The batch path
has no equivalent: its files land in `OUTPUT/JOB_ID/`, and the job id does not exist
until the job is submitted.

The `symbols` line is a free symbology lookup, and it says two things the price
cannot: how far the selection expands (`ES.FUT` is 37 instruments across that month,
which is why it costs what it does), and how many resolve for only part of the range -
those 5 are contracts that do not exist for the whole window. It is advisory: the
endpoint refuses some symbology combinations that fetch perfectly well, and when it
cannot answer, the line is simply absent and the quote stands.

## `--immediate`: stream it now

One plain streaming request, written straight to disk as
`DATASET.SCHEMA.SYMBOLS.START-END.KEY.dbn.zst`:

```
data/EQUS_MINI.ohlcv-1m.AAPL.20250602T000000-20250607T000000.49a8ed20dca417be.dbn.zst
```

DBN only - the streaming API does not deliver CSV or JSON. Streaming bills the moment
the request is issued.

The trailing `KEY` is a digest of the whole request - the same fields adoption matches
on - so two requests that differ in any of them get different files, and two spellings
of one request (symbols reordered, recased or repeated) get the same file. The bounds
carry seconds, and nanoseconds when they have them. The symbol segment is a
convenience for reading the directory: it is sanitized rather than validated (a symbol
is under no obligation to be a legal filename) and truncated with a count when the
list is long; anything it blurs, the key still distinguishes.

**The spend gate is weaker here.** The cost endpoint prices the batch feed mode only,
so the quote `--spend` is checked against is a floor: streaming is a dearer feed mode
and the actual charge is higher. dbnget says so on every `--immediate` run. Treat
`--spend` as an approximate ceiling on this path and an exact one on the batch path.

Because the request is already billed by the time a byte arrives, an existing file at
that name is an error rather than something to overwrite. Both the destination and its
`.part` sibling are claimed exclusively before the request is priced or issued, so two
concurrent runs of the same command cannot both pay. A run refused by the spend gate
releases both claims on the way out; if a claim is abandoned anyway - a killed
process, say - the next run says so in those terms and tells you to delete it, rather
than describing a payment that never happened. The stream lands on the `.part` sibling
and is renamed only once it completes, so a half-written file is never left under the
final name.

Every run states which path it took - `immediate mode: streaming directly, nothing is
queued on the account`, or `batch mode: reconciling against the jobs on the account` -
so a run that ends in a refusal still tells you whether a job was queued.

There is no session splitting, resume, or empty-day pre-pass here; with batch as the
default path, the vendor does the splitting.

## `dbnget list`

```sh
dbnget list                           # the batch jobs on the account
dbnget list --state queued,processing
dbnget list datasets                  # the datasets the account can access
```

Bare `list` lists jobs: ID, state, dataset, schema, symbols qualified by their
symbology, range, encoding and record limit, cost and size. It is the tool for
checking what already exists before submitting - a widened `--end` is a new job that
re-purchases the old range, and dbnget does not warn about overlaps.

```
JOB ID                    STATE  DATASET    SCHEMA    SYMBOLS             RANGE (UTC)                               OUTPUT                            COST      DATA   DOWNLOAD
GLBX-20260805-HAPEWPABKG  Done   GLBX.MDP3  tbbo      continuous:MNQ.v.0  2026-06-30T22:00:00..2026-07-31T21:00:00  csv out:instrument_id            $73.41   4.6 GiB  872.6 MiB
XNAS-20260803-SU6U8HRT75  Done   XNAS.ITCH  ohlcv-1m  raw_symbol:MSFT     2022-06-10T12:30:00..2022-06-10T14:00:00  csv out:instrument_id limit:1000  $0.00   8.1 KiB   10.3 KiB
```

`DATA` is the uncompressed record size and `DOWNLOAD` is the package that actually
comes down the wire - the number to size a disk or a wait against. Neither predicts
the other: compressed DBN packages several times smaller than its data, while a job of
a few small text files packages slightly larger.

The columns after the range are the fields adoption matches on that are not otherwise
visible. Bounds print as bare dates only when both fall on midnight; anything else
shows its times, down to the nanosecond when it has them, because adoption compares
exact instants. Encoding and output symbology always show; `limit` shows when set; and
anything else that differs from what dbnget itself submits is marked:

```
dbn out:instrument_id                                              <- a job dbnget made
csv out:instrument_id limit:1000 compression:none split:none       <- a job made elsewhere
```

dbnget always submits zstd compression and one file per day, and offers no flag for
`pretty_px`, `split_symbols` and their kin, so a job it created cannot vary in them
and its row stays quiet. A job created in the vendor's web UI can, and used to look
identical to an adoptable one while refusing to be adopted.

Long symbol lists are truncated with a count of what was left out.

## `dbnget get JOB_ID` - download a job by name

```sh
dbnget get XNAS-20260803-SU6U8HRT75 -o data
```

Downloads a finished job into `OUTPUT/JOB_ID/` and verifies every file, exactly as
adoption does. Reconciliation adopts a job only when the command reproduces the
original request field for field - the right rule for deciding whether to spend money
and a poor one for retrieving data already bought. When the original command is lost,
this is the way back to the data: take the ID from `dbnget list`.

Nothing on this path can charge: the job is already paid for and its files already
prepared. Re-running it is free and idempotent, since a file that verifies against the
manifest is kept rather than fetched again.

## `dbnget dataset` - one dataset's facts

```sh
$ dbnget dataset EQUS.MINI
range:   2023-03-28 0:00:00.0 +00:00:00 .. 2026-08-15 0:00:00.0 +00:00:00
schemas: mbp-1 tbbo trades bbo-1s bbo-1m ohlcv-1s ohlcv-1m ohlcv-1h ohlcv-1d definition
prices:  mbp-1 $1.20, tbbo $6.00, trades $6.00, bbo-1s $4.00, bbo-1m $4.00, ohlcv-1s $12.00, ohlcv-1m $12.00, ohlcv-1h $30.00, ohlcv-1d $30.00, definition $16.00 (USD per unit, historical)
```

The facts needed to turn a dataset code into a fetch command: bounds for
`--start`/`--end`, values for `-s`, and what each schema costs per unit. The names
follow the Databento API docs: `list datasets` for the many, `dataset` for the one.

The `prices` row is what makes two datasets carrying the same symbol comparable before
you buy either: `ohlcv-1m` is $12.00 per unit here and $35.00 on DBEQ.BASIC, and
`list datasets` lists bare codes because codes are all the API returns. Prices are for
the historical feed mode, which is what both the batch and `--immediate` paths draw
on. The row is advisory: if the endpoint declines, the rest of the card still prints.

`--publishers` shows the dataset's publisher table instead - venue by venue, the
decoding of the `publisher_id` field in downloaded records. dbnget filters the global
upstream listing to the one dataset, because OPRA alone has twenty-odd publishers:

```sh
$ dbnget dataset OPRA.PILLAR --publishers
20	AMXO	OPRA - NYSE American Options
21	XBOX	OPRA - BOX Options
...
```

## Symbology: `--stype-in` and `--stype-out`

`--stype-in` says how to read the symbols you passed. The same string means different
things in different symbologies, and the difference is what you are billed for:

| Symbology | A symbol means | Example |
|---|---|---|
| `raw_symbol` (default) | one exact symbol as the publisher writes it | `ESM4` is the June 2024 E-mini |
| `parent` | a whole product family | `ES.FUT` is every ES future |
| `continuous` | a rolling series, stitched across expiries | `ES.v.0` is front-month ES by volume |
| `instrument_id` | Databento's numeric ids | `3403` |

`ES.FUT` is the cautionary case: one instrument as a `raw_symbol`, the entire ES curve
as a `parent`. Same string, wildly different bill. There are eight more symbologies
for equities and cross-vendor identifiers, and `dbnget --help` lists them all.

`--stype-out` says how symbols appear in the delivered records. It defaults to
`instrument_id`, and the names are recoverable either way: DBN files carry the
id-to-symbol mappings in their metadata header, and CSV and JSON get a symbol column
by default. Leave it alone unless you know otherwise - symbol maps and the `--cost`
symbol lookup both require `instrument_id` on one side.

Both flags are part of what identifies a job, so changing either makes a different
request rather than the same one spelled differently.

Bounds carrying precision finer than a microsecond draw a warning: adoption compares
the vendor's echoed bounds to the nanosecond, and nothing promises a bound is echoed
exactly as sent. The request is legal and almost certainly fine - it just cannot be
proven so, and a possible re-purchase is worth a line of output.

### Finding out what exists

There is no free enumeration of a dataset. `dbnget list datasets` gives the dataset
codes and `dbnget dataset CODE` gives its range and schemas, but nothing lists the
instruments:

- The symbology lookup behind `--cost` resolves symbols you can already name. It will
  not enumerate: `ALL_SYMBOLS` is refused outright on GLBX.MDP3.
- A `parent` selection does expand to its family, which is the closest thing to free
  enumeration - `--cost` reports the count.
- The complete answer is the `definition` schema, one record per instrument carrying
  its symbol, product code, expiry and contract terms. That is a normal data request,
  so it is billed, but it is a per-day catalogue rather than a tick stream and
  `--cost` will price it first.

## Verified downloads

dbnget re-checks size and SHA-256 against the job manifest after every download and
fails if either disagrees, so a corrupt file cannot be mistaken for a complete one by
whatever runs next. (The client's own checksum pass warns and returns success on a
mismatch.) Files ending in `.zst` are also checked for a zstd frame header, and
manifest filenames are rejected unless they are plain file names, since they are
joined onto the output directory. Files are downloaded one at a time, each from the
manifest dbnget validated itself and each verified before the next one starts.

An interrupted download resumes: a file shorter than the manifest says is continued
from where it stopped, not re-fetched from zero. A file that is the right length but
fails its checksum is discarded and re-fetched, because the client skips a
length-matching file without reading it. If a short file's existing bytes turn out to
be corrupt, the resume completes it and the next run discards it under that same rule:
two runs, no manual intervention.

While it downloads, a run holds an exclusive lock on `OUT/JOB_ID/`, recorded in a
`.dbnget-lock` file there. A second run finding the directory held says so and exits 3
rather than waiting - exit 3 already means "not finished, come back" - and the hold is
what makes resuming safe: a short file can only be an interrupted transfer, never one
in progress elsewhere. The lock is advisory and released by the operating system when
the process ends, crash or kill included, so there is never a stale lock to clear by
hand; the `.dbnget-lock` file itself is inert and re-used.

## Verbosity

`-v` is dbnget at debug, which is where the reconcile steps are: how many jobs were
listed, whether one matched, whether anything was priced. `-vv` adds trace. `-vvv`
also turns on the vendor client's own logging, held back that far because its spans
carry the entire client struct on every line and drown everything else. `RUST_LOG`
overrides all of them.

## Development

```sh
cargo clippy --all-targets
cargo test
cargo fmt
```

The lint list in `Cargo.toml` is strict on purpose, and clippy is expected to pass
clean.

## License

MIT. See [LICENSE](LICENSE).
