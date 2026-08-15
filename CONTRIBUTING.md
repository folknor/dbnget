# Contributing

Bug reports, feature requests and pull requests are all welcome.

This is a small crate with one job, and the shape of it is documented in
[README.md](README.md). Read that first - most questions about how something behaves
are answered there, and it is kept accurate.

## Reporting a bug

The most useful report contains the command you ran, what you expected, and what
happened instead. `-v` adds dbnget's own debug logging, which shows the reconcile
steps: how many jobs were listed, whether one matched, whether anything was priced.

**Never paste your API key.** Not into an issue, not into a log excerpt, not into a
screenshot. Keys start with `db-` and are trivially greppable once public. If you think
one has leaked, revoke it in the Databento portal immediately.

The reports that led to most of this tool's behaviour were of the form "the output told
me X, and X was not true". Those are excellent. A command that costs money and refuses
to explain itself is a bug even when the refusal is technically correct.

## Building and testing

Requires Rust 1.97 or newer (edition 2024).

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt
```

All four must be clean before a pull request. The lint list in `Cargo.toml` is
deliberately strict - `unwrap_used`, `cast_possible_truncation`, `float_cmp`,
`too_many_lines`, `cognitive_complexity` and friends are all `deny`. **Fix the code
rather than loosening the lint.** `#[expect(..., reason = "...")]` is acceptable in
tests, and in production code only with a reason that explains why the lint is wrong
here specifically.

## The one rule that matters most

**Never write a test that submits a batch job.** A submit is a real purchase against
whoever runs the suite. Tests cover pure functions - parsing, matching, verification,
formatting, rendering - which is where nearly all the logic that can be wrong actually
lives.

More generally: this tool spends money on behalf of people who are not in this
repository. Three areas carry that weight directly, and a change to any of them needs to
say in the commit message why it is safe:

- **The spend gate** (`spend.rs`). It defaults to a $0.00 cap. Never weaken, skip, or
  default-bypass it. Rounding a comparison, widening a tolerance, or treating an
  unparseable quote as free are all ways to charge someone without their approval.
- **Job matching** (`jobs.rs`). Re-running a command adopts the job that already bought
  the data instead of buying it again. Every field that changes the delivered bytes has
  to be compared, so a field added to a submission and not to the match key is a silent
  double charge.
- **Output paths** (`fetch.rs`, `verify.rs`). Filenames come from vendor responses and
  from the command line, and they are joined onto the output directory. Data that has
  been paid for must never be overwritten, and a corrupt file must never be mistakable
  for a complete one.

If you are unsure whether a change touches one of these, open an issue before writing
it. "I did not realise that counted as a charge" is a fine thing to say beforehand and a
painful one afterwards.

## Style

Match the surrounding code. A few specifics that are easy to trip over:

- Comments explain **why**, not what. The what is in the code directly above.
- Plain ASCII punctuation. No em-dashes, en-dashes, curly quotes or ellipsis
  characters.
- A user-visible change - a flag, an output, a behaviour, a refusal - gets a
  `CHANGELOG.md` entry in the same commit that makes it. Internal refactors that change
  nothing observable do not.
- Documentation is binding. If you change behaviour that `README.md` describes, change
  `README.md` in the same commit.

## LLM-assisted contributions

This project is built with LLM coding agents, and contributions written the same way are
welcome. See [LLM.md](LLM.md) for how that works and why it is documented.

One constraint follows from it and applies to every contributor, tooling or not: **do
not copy code from other implementations**, and do not paste third-party source into an
agent session that is writing code for this repository. The project maintains a
structural separation between sessions that read other implementations and sessions that
write this one. A contribution that has been through a differently-licensed codebase on
its way here cannot be accepted, and it is not always possible to tell after the fact -
which is why the separation is kept up front.

Whatever wrote it, you are responsible for understanding what you submit and for being
able to explain why it is correct.

## Licence

Contributions are accepted under the [MIT licence](LICENSE), the same terms the project
is distributed under.
