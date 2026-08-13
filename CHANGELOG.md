# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Job matching now compares every output-affecting submission field (`pretty_px`,
  `pretty_ts`, `map_symbols`, `split_size`, `delivery`), resolving `map_symbols`
  against its encoding-dependent default first. A job delivering different bytes could
  previously be adopted as an exact match.
- Symbol selections are compared as true sets. Sorting without deduplicating meant a
  request naming a symbol twice failed to match the job it had already bought, and
  re-purchased it.
- Batch files are downloaded one at a time by validated name. Handing the whole job to
  the client's download-all re-fetched the manifest internally and built paths from
  that second response, so the validated filenames were not the ones written.
- The free-space floor is checked before every file and accounts for that file's size,
  rather than once per job.
- The job id is validated as a path component before being used as one.
- An empty job already on the account is refused on adoption, and a done job with an
  empty manifest is an error. Both previously exited 0.
- `--immediate` filenames carry seconds and nanoseconds, so two requests differing
  below the minute no longer collide and silently truncate each other; an existing
  destination is refused; the stream lands on a `.part` sibling renamed on success;
  and a dataset containing a path separator can no longer escape `--output`.
- Destinations that are symbolic links are refused before anything is written.
- `dbnget dataset CODE --publishers` errors on a code with no publishers instead of
  printing nothing and exiting 0.
- `--end` is documented as exactly 24 hours after `--start` for timestamp starts too,
  which is what it always did.

## [0.1.0] - 2026-08-13

Initial release.

[0.1.0]: https://github.com/folknor/dbnget/releases/tag/v0.1.0
