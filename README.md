# pacpurge

A terminal interface for finding and reclaiming space on Arch and EndeavourOS.

pacpurge answers the question `pacman -Qi` cannot: **which of these packages do
I not actually use, and how much would removing them really free?** It reads
the local pacman database directly, works out what each package drags along
with it, and infers when each one was last used from the access times on its
binaries and libraries.

```
╭ 1204 of 1204 packages ─────────────────────────────────────────────╮╭ details ───────────────────────╮
│   package            size       frees  last used  added    origin  ││android-studio                  │
│   android-studio    2.8 GiB     3.4 GiB     never   1.1y  aur/local││2024.1.1.12-1                   │
│   texlive-full      1.9 GiB     1.9 GiB      412d   2.0y      extra││                                │
│ ● cuda              1.2 GiB     2.9 GiB     never   287d      extra││installed size    2.8 GiB       │
│   qt6-base            412 MiB     412 MiB       2d   1.0y      extra││frees if removed  3.4 GiB       │
│·  glibc                47 MiB      47 MiB       1d   1.4y       core││  not read since it was         │
╰────────────────────────────────────────────────────────────────────╯╰────────────────────────────────╯
 o orphans   a aur   e explicit   n never-used   u stale  sort:size↓   ? help   space mark   Enter apply
```

## What it tells you that `pacman` does not

**`frees` is not `size`.** A 40 MiB package that is the last thing needing a
400 MiB toolchain frees 440 MiB, because `pacman -Rns` takes the toolchain
with it. pacpurge simulates that cascade for every installed package and sorts
by the real figure. The column turns green when a package drags others along.

**`last used` is real evidence.** Every file carries an access time. Arch
mounts filesystems `relatime` by default, so that timestamp updates at most
once a day on read — useless for profiling, exactly right for *"has anything
touched this in the last six months?"*.

Each package is judged by the strongest evidence it actually ships, in tiers:
its executables if it has any, otherwise its shared libraries, otherwise the
data it installs — fonts, icon themes, a TeX tree. That last tier matters more
than it sounds: the biggest packages on a desktop ship no binary at all, so
judging only by binaries left exactly the packages you most want to inspect
with a blank column. The tiers are not mixed, so a stray read of one data file
cannot vouch for a binary that has never run.

Documentation is the one thing excluded outright. An indexer reading a man
page says nothing about whether you use the software, so a package that ships
*only* documentation honestly reports `n/a` rather than guessing.

Two things it refuses to fudge:

- Package extraction stamps an access time at install. A package that has
  never been run therefore looks "used" on its install date. pacpurge compares
  the two and reports **`never`** — not read since it was installed. On an AUR
  package, that usually means *you tried it once*.
- On a `noatime` mount the timestamps are frozen. Rather than print a stale
  date as though it meant something, pacpurge turns the column off and says
  why.

**Orphans are computed, not guessed.** The dependency graph is built from the
local database with `provides` resolved and version constraints stripped, so a
dependency satisfied by a virtual name still counts as needed.

## Beyond packages

The Reclaim tab covers the space that is usually much larger than any package:

| Target | Why it is there |
| --- | --- |
| Superseded package archives | Every version you ever upgraded through, still in `/var/cache/pacman/pkg`. The installed version and one rollback are kept. |
| Archives for removed packages | Downloads for packages that are not installed at all. |
| AUR helper build caches | `yay`, `paru`, `pikaur`, `trizen`, `aurutils` keep a git clone and a full build tree for every package they ever built. |
| Orphaned dependencies | Installed to satisfy something that is now gone. |
| AUR packages never used since install | The cross-section that is almost always safe to drop. |
| Debug symbol packages | Only useful while debugging that exact build. |
| Module trees from removed kernels | `/usr/lib/modules/<release>` left behind when a kernel package goes. pacman does not own these files, so nothing else cleans them up. |
| `.pacnew` / `.pacsave` files | Configuration merges you never finished. |
| Systemd coredumps and journal | Crash dumps and logs, with a vacuum command sized for you. |
| Language toolchain caches | `~/.cargo/registry`, `~/.npm/_cacache`, Go, Gradle, Maven, pip, uv. Nothing ever prunes these. |
| Desktop trash | Files you already deleted once. |
| Unused flatpak runtimes | Advisory only — flatpak alone knows which runtimes are unreferenced, so pacpurge does not invent a size for it. |

Each target is labelled `safe`, `review` or `careful`, and shows the exact
command it would run before it runs it.

## Safety

pacpurge never deletes a package file itself. Removals are handed to
`pacman -Rns`, which applies its own dependency resolution and its own
confirmation prompt — pacpurge's review screen is a second pair of eyes, not a
replacement for pacman's.

Beyond that:

- **Protected packages.** Everything the `base` group depends on, plus the
  kernel, bootloader, `sudo` and `pacman`, is marked and cannot be selected by
  a normal keystroke. `P` overrides it deliberately.
- **Breakage is offered a fix, not worked around.** If the selection would
  strand installed packages, pacpurge names them and offers to mark them too —
  the whole transitive closure at once, with the new total, so you are not
  discovering it one refusal at a time. Accepting takes you straight to the
  confirmation. If that closure reaches the base system, the offer is withheld
  and pacpurge says so instead: uninstalling half the machine should not be one
  keystroke away.
- **`--dry-run`** prints what would run and executes nothing.
- Reported sizes are truncated, never rounded up, so a promised figure is
  never larger than the space you get back.

## Install

Straight from the repository, no clone needed:

```bash
cargo install --git https://github.com/ilvar/pacpurge --locked
```

Or from a checkout:

```bash
git clone https://github.com/ilvar/pacpurge
cd pacpurge
cargo install --path . --locked
```

Or build the static binary directly:

```bash
cargo release-small     # x86_64-unknown-linux-musl, ~900 KB, no runtime deps
```

`cargo install` puts the binary in `~/.cargo/bin`, which needs to be on your
`PATH`.

pacpurge needs no privileges to scan. It asks for `sudo` only at the moment it
runs a command that requires it.

## Use

```bash
pacpurge                       # the interactive interface
pacpurge --list                # print the package table and exit
pacpurge --clean               # print the reclaim targets and exit
pacpurge --json | jq .summary  # the whole analysis as one JSON document
```

Useful options:

```
--top <N>          probe access times for only the N largest packages.
                   Defaults to 0, meaning every package — a full probe of a
                   2000-package system costs about a third of a second. When
                   set, AUR packages and orphans are probed regardless of
                   size, because an unused 12 MiB AUR build is a better
                   candidate than a 400 MiB toolchain in daily use.
--stale-days <N>   days without a read before a package counts as stale (180)
--no-usage         skip access-time probing entirely
--quick            skip directory size measurement, the slow part of a scan
--root <PATH>      analyse another filesystem root — a mounted disk, a chroot
--dry-run          never execute anything; print the commands instead
```

### Keys

| | |
| --- | --- |
| `j` `k` `↑` `↓` `Ctrl-D` `Ctrl-U` `g` `G` | move |
| `Tab` | switch between Packages and Reclaim |
| `space` | mark the package under the cursor |
| `P` | mark it even though it is part of base |
| `Enter` | review and run what is marked |
| `o` `a` `e` `n` `u` | filter: orphans, AUR, explicit, never-used, stale |
| `p` | hide packages the base system depends on |
| `/` | filter by package name |
| `D` | search descriptions as well as names |
| `Esc` | clear every filter |
| `s` `S` `1`–`6` | sort: next column, reverse, or pick one |
| `r` | re-scan |
| `?` | keys |

Filters compose with AND. `a` then `n` is the list worth looking at first:
AUR packages you installed and never opened.

`/` matches package names, because searching `lib` should find the packages
*called* `lib…` rather than the several hundred whose description happens to
mention the word. `D` widens it to descriptions when that is what you want,
and the search field shows which it is currently matching.

## How it works

| Module | Responsibility |
| --- | --- |
| `capability` | Every filesystem, `stat` and subprocess call in the program. |
| `localdb` | Parses `/var/lib/pacman/local/*/desc` and `files`. |
| `graph` | Dependency edges, orphans, and the `pacman -Rns` cascade simulation. |
| `usage` | Access times to a last-used verdict, including the `noatime` case. |
| `janitor` | Reclaimable space outside the package set. |
| `filter` | Sorting and filtering the table. |
| `plan` | A selection to the exact commands that will run. |
| `scan` | The one place those pieces meet the filesystem. |
| `app` / `keys` / `ui` | State machine, keymap, renderer. |

Everything except `capability` and `scan` is pure, which is why the suite runs
anywhere: the parsing, graph and planning logic is tested directly, and the
end-to-end tests build a synthetic pacman root — real files, real access times
— rather than mocking the filesystem.

pacpurge reads the local database rather than shelling out to `pacman -Qi`
per package, which is roughly two orders of magnitude faster. `pacman` itself
is called exactly once, for `pacman -Sl`, to learn which repository each
package came from and therefore which are foreign. Without pacman on `PATH`,
that column reads `?` and a warning says so.

## Development

Built on [strictrs](https://github.com/ilvar/strictrs), a strict subset of
Rust: no `unsafe`, no `unwrap`/`expect`/indexing outside tests, no numeric
`as` casts, no glob imports, explicit return types, and filesystem and process
effects confined to a module marked `// strictrs: capability`.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
strictrs check .          # must report "ok": true
```

See [`AGENTS.md`](AGENTS.md) for the project-specific rules.

## Limitations

- **Access times are a proxy, not a log.** A library loaded by something else
  reads as used even if you never invoked the package's own binary. The signal
  is strong in the negative direction — `never` is reliable — and weaker in
  the positive one. Anything that walks the whole filesystem — a backup, an
  indexer, `updatedb` — can also refresh access times wholesale; if every
  package suddenly reads as used on the same day, that is what happened.
- **Dependency cycles are not swept up.** Two packages that require each other
  keep each other alive. pacman's own `-Rns` and `-Qdt` behave the same way,
  and reporting space that pacman will not actually reclaim would be worse
  than under-reporting it.
- **No general untracked-file scan.** Post-install hooks legitimately create
  files pacman does not own — font caches, icon caches, `ldconfig` output —
  and a scan that flagged them would be mostly false positives. Only kernel
  module trees, where `pkgbase` gives a definitive answer, are reported.
- **`provides` versions are not compared.** A dependency satisfied by a
  virtual name matches every provider of that name, which over-estimates how
  needed a package is. That is the safe direction to be wrong in.
