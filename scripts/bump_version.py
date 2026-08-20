#!/usr/bin/env python3
"""Pre-commit hook: bump the patch version when the shipped artifact changes.

Every merge to main publishes `v<version>`. Without a bump that tag is silently
overwritten and a restart can pick up code nobody expected, so the version has to
move whenever the released binary would.

Two files carry the version and are kept in step:
  * Cargo.toml — the source of truth
  * Cargo.lock — CI builds with --locked, so a stale lock fails the build
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CARGO_TOML = ROOT / "Cargo.toml"
CARGO_LOCK = ROOT / "Cargo.lock"
PACKAGE = "pacpurge"

# A change to any of these ends up inside the released binary.
TRIGGER_DIRS = ("src/", "tests/", ".cargo/")
TRIGGER_FILES = ("Cargo.toml", "rust-toolchain.toml")

VERSION_RE = re.compile(r'^version = "(\d+\.\d+\.\d+)"$', re.MULTILINE)


def staged_files() -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--cached", "--name-only"],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line for line in result.stdout.splitlines() if line]


def version_in(text: str) -> str | None:
    match = VERSION_RE.search(text)
    return match.group(1) if match else None


def already_bumped() -> bool:
    """True if the pending commit's version already differs from HEAD's."""
    result = subprocess.run(
        ["git", "show", "HEAD:Cargo.toml"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return False
    head = version_in(result.stdout)
    current = version_in(CARGO_TOML.read_text())
    return head is not None and current is not None and head != current


def should_bump(files: list[str]) -> bool:
    if not files:
        print("bump_version: nothing staged, skipping")
        return False

    if already_bumped():
        print("bump_version: version already moved since HEAD, skipping")
        return False

    for path in files:
        if path.startswith("scripts/"):
            continue
        if path.startswith(TRIGGER_DIRS) or path in TRIGGER_FILES:
            print(f"bump_version: {path} changed")
            return True

    print("bump_version: nothing that reaches the binary changed, skipping")
    return False


def bump(old: str) -> str:
    major, minor, patch = (int(part) for part in old.split("."))
    return f"{major}.{minor}.{patch + 1}"


def rewrite_cargo_toml() -> tuple[str, str]:
    content = CARGO_TOML.read_text()
    match = VERSION_RE.search(content)
    if not match:
        raise SystemExit("bump_version: no version found in Cargo.toml")

    old = match.group(1)
    new = bump(old)
    CARGO_TOML.write_text(
        content[: match.start(1)] + new + content[match.end(1) :],
    )
    return old, new


def rewrite_cargo_lock(old: str, new: str) -> None:
    """Only this package's entry — every other pin must stay untouched."""
    content = CARGO_LOCK.read_text()
    pattern = re.compile(
        r'(\[\[package\]\]\nname = "' + re.escape(PACKAGE) + r'"\nversion = ")'
        + re.escape(old)
        + r'(")'
    )
    updated, count = pattern.subn(r"\g<1>" + new + r"\g<2>", content)
    if count != 1:
        raise SystemExit(
            f"bump_version: expected one {PACKAGE} entry in Cargo.lock, found {count}"
        )
    CARGO_LOCK.write_text(updated)


def main() -> int:
    try:
        files = staged_files()
    except subprocess.CalledProcessError:
        print("bump_version: git diff failed, skipping")
        return 0

    if not should_bump(files):
        return 0

    old, new = rewrite_cargo_toml()
    rewrite_cargo_lock(old, new)

    subprocess.run(
        ["git", "add", str(CARGO_TOML), str(CARGO_LOCK)],
        check=True,
    )
    print(f"bump_version: {old} -> {new}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
