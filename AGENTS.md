# AGENTS.md

Guidance for AI agents and contributors. This project was scaffolded by
`strictrs new` and is built on the strictrs strict Rust profile.

## Commands

```bash
strictrs check .                                             # strict profile oracle (JSON)
cargo fmt
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

`strictrs check .` must report `"ok": true` before a change is done. Do not
suppress or mask diagnostics.

## Hard constraints (enforced by strictrs + lints)

- No `unsafe`.
- No panicking APIs in non-test code: no `unwrap`, `expect`, or unchecked
  indexing. Return/handle `Result`/`Option`; use `.get(...)`, `ok_or_else`, `?`.
- No numeric `as` casts — use `TryFrom`/`From`.
- No glob imports — import each item explicitly.
- Every public function has an explicit return type.
- Handle or explicitly discard `#[must_use]` values.
- Keep filesystem/network/process/environment access inside a module preceded by
  the exact comment `// strictrs: capability`.
- Match arms over locally-defined enums must name every variant; no `_` catch-all.

## Tooling

- `.pre-commit-config.yaml` mirrors the CI gate (fmt, clippy, test, strictrs,
  shellcheck, bump-version). Run `pre-commit run --all-files` before pushing.
- `scripts/commit.sh "msg"` runs that gate, bumps the version, and commits
  (`-p` to push, `-n` to skip checks).
- `scripts/bump_version.py` patch-bumps the version when a staged file reaches
  the image, so the moving `v<version>` image tag is never overwritten in place.
- `.github/workflows/ci.yml` compiles once, tests, runs `strictrs check`, then
  builds the static MUSL binary and smoke-tests it against a synthetic pacman
  root.
- There is no `Dockerfile`. pacpurge inspects the host's pacman database,
  its package cache and its mount options; a container has none of those, so a
  distroless image would only ever prove that `--help` prints. The MUSL job
  replaces it and checks something real.


## What this project is

pacpurge finds space worth reclaiming on an Arch system and helps take it
back. It is a diagnostic tool that happens to be able to act, not a package
manager.

## Architectural rules

- **Effects live in `src/capability.rs`, and nowhere else.** Every `fs`,
  `stat` and subprocess call in the program is inside the module marked
  `// strictrs: capability`. `scan.rs` is the only module that both calls into
  it and knows about the analysis passes. If a new feature needs to read
  something, add a function to the capability module rather than reaching for
  `std::fs` where you are.
- **The analysis is pure.** `localdb`, `graph`, `usage`, `janitor`, `filter`,
  `plan` and `format` take values and return values. That is what makes the
  suite runnable on a machine that is not Arch, so keep it that way.
- **`app.rs` performs nothing.** `App::handle` returns an `Action` and `main`
  decides what to do about it. Running a command means leaving the alternate
  screen, which only `main` may do.
- **`keys.rs` exists so the state machine never sees a raw key.** It is also
  the one file allowed a catch-all match arm, because `KeyCode` is not ours
  and gains variants that this program has no opinion about.

## Correctness rules specific to this domain

- **Never round a reclaim figure up.** `format::bytes` truncates. A number
  that promises more space than the user gets back is a bug, and there is a
  property test asserting it.
- **Never report a last-use verdict without evidence for it.** `noatime`
  freezes access times; the column is disabled rather than filled with stale
  dates. An access time at install time means *never used*, not *used then*.
- **Judge a package by the strongest tier it ships, and do not mix tiers.**
  Executables, then libraries, then installed data. Narrowing the witness set
  to binaries and libraries left every font, icon theme and TeX package — the
  largest things on a desktop — with no verdict at all, which is the failure
  mode to watch for when touching `usage::WITNESS_TIERS`. Taking the newest
  access time across mixed tiers is the other failure mode: a stray read of
  one data file would vouch for a binary that has never run. Documentation
  stays excluded outright; an indexer reading a man page is not evidence.
- **Never claim space pacman will not actually free.** The cascade simulation
  mirrors `pacman -Rns`: explicitly installed packages are not swept up,
  protected ones are not swept up, and dependency cycles are left alone
  because pacman leaves them alone. Matching pacman matters more than
  reporting a bigger number.
- **Never delete package files directly.** Removals go to `pacman -Rns`, which
  runs its own checks and its own prompt. Direct deletion is only for caches
  and leftovers that pacman does not own.
- **Never widen the untracked-file scan.** Post-install hooks create files
  pacman does not own; flagging them would be mostly false positives. Kernel
  module trees are reported only because `pkgbase` gives a definitive answer.

## Known tool interaction

`strictrs::explicit_return_type` wants an explicit `-> ()` on public
functions, and `clippy::unused_unit` — an error here under `-D warnings` —
wants it removed. The two cannot both be satisfied, so public functions in
this crate return a meaningful value instead of unit. Where that would be
contrived, prefer making the function private over adding an `#[allow]`; most
of the mutators return `bool` for "something changed", which the render loop
uses anyway.

## Testing rules

Every behaviour change needs a test at the right layer:

- **Pure logic** — a unit test in the module's own `mod tests`.
- **Anything touching the filesystem** — a case in `tests/end_to_end.rs`,
  which builds a real synthetic pacman root under `target/test-roots/`, writes
  real files and sets real access times with `File::set_times`. Do not mock
  the filesystem; the point of these tests is that the kernel is in the loop.
- **Anything drawn** — a case in `tests/render.rs` using ratatui's
  `TestBackend`. Reading the cells back catches what state-machine tests
  cannot: a panicking layout, a column padded wider than its constraint, a
  value that reaches the screen truncated.
- **Anything with an invariant** — a property in `tests/properties.rs`. The
  cascade simulation and the size formatter both have properties that would
  catch a regression no example test would.

When adding a table column, add its padding width as a constant next to its
layout constraint. A padding width wider than the constraint silently chops
the last characters off the value, which is invisible until someone reads a
truncated size.
