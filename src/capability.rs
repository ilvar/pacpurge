//! The single boundary through which this program touches the outside world.
//!
//! Every filesystem read, directory walk, `stat`, deletion and subprocess
//! lives inside the marked module below. Nothing above this layer performs an
//! effect, which is what makes the parsing, dependency and planning code
//! testable without an Arch system to point it at.

/// Metadata read back from a single path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    /// Apparent size in bytes.
    pub size: u64,
    /// Actual space used on disk, from the allocated block count.
    pub disk_size: u64,
    /// Last access time, Unix seconds.
    pub atime: i64,
    /// Last modification time, Unix seconds.
    pub mtime: i64,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Whether the path is a symbolic link.
    pub is_symlink: bool,
}

/// One entry from a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry's own name, without any directory part.
    pub name: String,
    /// Full path to the entry.
    pub path: std::path::PathBuf,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// Total space taken by a directory tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes actually allocated on disk.
    pub bytes: u64,
    /// Number of regular files seen.
    pub files: usize,
    /// Whether the walk stopped early at the node budget.
    pub truncated: bool,
}

/// The result of running a subprocess to completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Exit status, or `None` if the process was killed by a signal.
    pub code: Option<i32>,
    /// Captured standard output, lossily decoded as UTF-8.
    pub stdout: String,
    /// Captured standard error, lossily decoded as UTF-8.
    pub stderr: String,
}

impl Output {
    /// Whether the process exited successfully.
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

// strictrs: capability
mod effects {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Entry, Output, Stat, Usage};

    /// Read a file as UTF-8, replacing invalid sequences.
    ///
    /// Pacman writes its database in UTF-8, but a package description from a
    /// mangled AUR build can still be invalid. Losing a character beats losing
    /// the package from the report.
    pub fn read_text(path: &Path) -> io::Result<String> {
        let bytes = fs::read(path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// List a directory, sorted by name for deterministic output.
    pub fn list_dir(path: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();

        for item in fs::read_dir(path)? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            let is_dir = item.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            entries.push(Entry {
                name,
                path: item.path(),
                is_dir,
            });
        }

        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    /// Stat a path without following symbolic links.
    ///
    /// Not following links matters for the access-time probe: reading through
    /// a symlink updates the target's atime, and it is the target that carries
    /// the evidence.
    pub fn stat(path: &Path) -> Option<Stat> {
        let metadata = fs::symlink_metadata(path).ok()?;
        Some(Stat {
            size: metadata.size(),
            // `blocks()` counts 512-byte units regardless of filesystem block
            // size, which is what `du` reports and what actually gets freed.
            disk_size: metadata.blocks().saturating_mul(512),
            atime: metadata.atime(),
            mtime: metadata.mtime(),
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
        })
    }

    /// Whether a path exists, following symbolic links.
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Sum the disk usage of a directory tree.
    ///
    /// Stays on one filesystem, never follows symbolic links, and stops after
    /// `budget` entries so that pointing this at a home directory cannot hang
    /// the UI. Hard-linked files are counted once.
    pub fn tree_usage(root: &Path, budget: usize) -> Usage {
        let mut usage = Usage::default();
        let mut seen_inodes: BTreeSet<(u64, u64)> = BTreeSet::new();
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        let mut visited = 0usize;

        let root_device = fs::symlink_metadata(root)
            .ok()
            .map(|metadata| metadata.dev());

        while let Some(directory) = stack.pop() {
            let Ok(children) = fs::read_dir(&directory) else {
                continue;
            };

            for child in children.flatten() {
                visited = visited.saturating_add(1);
                if visited > budget {
                    usage.truncated = true;
                    return usage;
                }

                let Ok(metadata) = fs::symlink_metadata(child.path()) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if root_device.is_some_and(|device| device != metadata.dev()) {
                    continue;
                }

                if metadata.is_dir() {
                    stack.push(child.path());
                    continue;
                }

                if metadata.nlink() > 1 && !seen_inodes.insert((metadata.dev(), metadata.ino())) {
                    continue;
                }

                usage.bytes = usage
                    .bytes
                    .saturating_add(metadata.blocks().saturating_mul(512));
                usage.files = usage.files.saturating_add(1);
            }
        }

        usage
    }

    /// The newest modification time anywhere under `root`.
    ///
    /// Modification times are never suppressed by mount options, which is what
    /// makes this usable on a `noatime` filesystem where access times are
    /// frozen. Bounded by `budget` entries, and returns the path that carried
    /// the timestamp so the interface can show its evidence.
    pub fn newest_mtime(root: &Path, budget: usize) -> Option<(i64, PathBuf)> {
        let mut best: Option<(i64, PathBuf)> = None;
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        let mut visited = 0usize;

        if let Ok(metadata) = fs::symlink_metadata(root) {
            best = Some((metadata.mtime(), root.to_path_buf()));
        }

        while let Some(directory) = stack.pop() {
            let Ok(children) = fs::read_dir(&directory) else {
                continue;
            };

            for child in children.flatten() {
                visited = visited.saturating_add(1);
                if visited > budget {
                    return best;
                }

                let Ok(metadata) = fs::symlink_metadata(child.path()) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }

                let mtime = metadata.mtime();
                let newer = match &best {
                    Some((current, _path)) => mtime > *current,
                    None => true,
                };
                if newer {
                    best = Some((mtime, child.path()));
                }

                if metadata.is_dir() {
                    stack.push(child.path());
                }
            }
        }

        best
    }

    /// Collect paths under `root` whose file name ends with one of `suffixes`.
    ///
    /// Bounded by `budget` for the same reason as [`tree_usage`].
    pub fn find_by_suffix(root: &Path, suffixes: &[&str], budget: usize) -> Vec<(PathBuf, u64)> {
        let mut found = Vec::new();
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        let mut visited = 0usize;

        while let Some(directory) = stack.pop() {
            let Ok(children) = fs::read_dir(&directory) else {
                continue;
            };

            for child in children.flatten() {
                visited = visited.saturating_add(1);
                if visited > budget {
                    found.sort();
                    return found;
                }

                let Ok(metadata) = fs::symlink_metadata(child.path()) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_dir() {
                    stack.push(child.path());
                    continue;
                }

                let path = child.path();
                let matches = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| suffixes.iter().any(|suffix| name.ends_with(suffix)));
                if matches {
                    found.push((path, metadata.blocks().saturating_mul(512)));
                }
            }
        }

        found.sort();
        found
    }

    /// Delete a file, or a directory and everything under it.
    pub fn remove(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    /// Run a command, capturing its output.
    pub fn run_captured(program: &str, args: &[String]) -> io::Result<Output> {
        let output = process::Command::new(program).args(args).output()?;
        Ok(Output {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run a command with the terminal attached, and wait for it.
    ///
    /// Used for the destructive commands: `sudo` needs a real terminal to
    /// prompt for a password, and pacman's own confirmation prompt is a
    /// better last line of defence than anything this program could draw.
    pub fn run_interactive(program: &str, args: &[String]) -> io::Result<Option<i32>> {
        let status = process::Command::new(program).args(args).status()?;
        Ok(status.code())
    }

    /// Whether an executable is resolvable through `PATH`.
    pub fn has_program(program: &str) -> bool {
        let Ok(path) = std::env::var("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    }

    /// The effective user id of this process.
    ///
    /// Read from the process's own `/proc` entry so that no libc binding is
    /// needed for the one number this program cares about.
    pub fn effective_uid() -> u32 {
        fs::metadata("/proc/self")
            .map(|metadata| metadata.uid())
            .unwrap_or(u32::MAX)
    }

    /// The invoking user's home directory.
    ///
    /// Under `sudo`, `$HOME` still points at the original user on most
    /// configurations, but `$SUDO_USER` is checked first so that scanning
    /// per-user caches finds the right person's files.
    pub fn home_dir() -> Option<PathBuf> {
        if let Ok(user) = std::env::var("SUDO_USER") {
            let candidate = PathBuf::from("/home").join(&user);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        std::env::var_os("HOME").map(PathBuf::from)
    }

    /// Current wall-clock time in Unix seconds.
    pub fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .unwrap_or_default()
    }
}

pub use effects::{
    effective_uid, exists, find_by_suffix, has_program, home_dir, list_dir, newest_mtime, now,
    read_text, remove, run_captured, run_interactive, stat, tree_usage,
};
