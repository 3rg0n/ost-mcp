# Contributing

## Quality gate

Everything must pass before a change lands, across the whole workspace and not
just the files you touched:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
```

Report the command and its output, not a claim that it passed.

## Never commit mailbox data

This reader is pointed at real mail. A test fixture, a doc example, a commit
message and a changelog entry are all published, so none of them may contain a
real subject line, sender address, display name or attachment payload — use
`example.com` addresses and invented names. `.gitignore` covers `*.ost`, `*.pst`,
`*.nst` and dump output; do not redirect the `dump` example into the repo.

The same goes for a store path that carries a username or UPN. Resolve the path at
runtime instead of writing one down.

## Reverse-engineered format claims

Format version 36 is undocumented, so anything asserted about it has to be
*measured*, with the measurement recorded in `docs/ost-v36-format.md`. Say what
was counted and on what — "0x0037 resolves 6781/6781 rows at bit 19 vs 816/6781 at
bit 16" — rather than what the spec implies. Where measurement contradicts
MS-PST, say so explicitly; that is the useful part of the document.

A silent fallback that produces a plausible value is worse than a NULL. If a
property cannot be resolved, it is `None`.

## CHANGELOG (required)

Every user-facing or operational change gets an entry in `CHANGELOG.md` under
`## [Unreleased]`, following [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

- **Six section types, only these:** `Added`, `Changed`, `Deprecated`, `Removed`,
  `Fixed`, `Security`. A dependency bump is `Changed`, or `Security` when it closes
  an advisory. A docs change is `Changed`.
- **One bullet, one change, one or two sentences.** No sub-bullets, no invented
  headings (`Why`, `Verified`, `Impact`). Measurements and reasoning belong in the
  commit message, the PR, or `docs/`.
- **Claim first.** Put the outcome in the opening sentence — anything summarising
  the changelog keeps that sentence and drops the rest.
- **Never date your own entry.** It goes under `[Unreleased]`; the maintainer moves
  it under a dated version heading when cutting a release.
- Skip it for a pure refactor nothing outside the codebase can observe.
