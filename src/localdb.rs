//! Parser for pacman's local database format.
//!
//! `/var/lib/pacman/local/<name>-<version>/desc` is a flat file of `%KEY%`
//! headers, each followed by one value per line until a blank line. Parsing it
//! directly is roughly two orders of magnitude faster than shelling out to
//! `pacman -Qi` for every package, and it keeps this crate free of a runtime
//! dependency on pacman's output format staying stable across versions.
//!
//! This module is pure: it turns text into values. Reading the files is
//! [`crate::capability`]'s job.

use std::path::Path;

use crate::model::{InstallReason, Package};

/// Split a `desc` file into `(key, values)` sections.
///
/// Unknown keys are preserved rather than dropped so that a pacman version
/// which adds a field does not silently lose data. Malformed input yields
/// fewer sections rather than an error: a single unreadable package must not
/// stop the whole scan.
pub fn sections(text: &str) -> Vec<(String, Vec<String>)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;

    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r');

        if trimmed.starts_with('%') && trimmed.ends_with('%') && trimmed.len() >= 2 {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let key = trimmed.trim_matches('%').to_owned();
            current = Some((key, Vec::new()));
            continue;
        }

        if trimmed.is_empty() {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            continue;
        }

        if let Some((_key, values)) = current.as_mut() {
            values.push(trimmed.to_owned());
        }
    }

    if let Some(section) = current.take() {
        sections.push(section);
    }

    sections
}

/// Parse a `desc` file into a [`Package`].
///
/// Returns `None` when the entry has no `%NAME%`, which is the only field
/// without which nothing downstream can work.
pub fn parse_desc(text: &str, db_dir: &Path) -> Option<Package> {
    let mut package = Package {
        db_dir: db_dir.to_path_buf(),
        ..Package::default()
    };
    let mut named = false;

    for (key, values) in sections(text) {
        let first = values.first().map(String::as_str).unwrap_or_default();
        match key.as_str() {
            "NAME" => {
                if first.is_empty() {
                    return None;
                }
                first.clone_into(&mut package.name);
                named = true;
            }
            "VERSION" => first.clone_into(&mut package.version),
            "DESC" => package.description = values.join(" "),
            "URL" => first.clone_into(&mut package.url),
            "PACKAGER" => first.clone_into(&mut package.packager),
            "SIZE" => package.size = first.parse::<u64>().unwrap_or_default(),
            "INSTALLDATE" => package.install_date = first.parse::<i64>().ok(),
            "BUILDDATE" => package.build_date = first.parse::<i64>().ok(),
            "REASON" => {
                package.reason = if first == "1" {
                    InstallReason::Dependency
                } else {
                    InstallReason::Explicit
                };
            }
            "GROUPS" => package.groups = values,
            "DEPENDS" => package.depends = values,
            "OPTDEPENDS" => package.optdepends = values,
            "PROVIDES" => package.provides = values,
            "REPLACES" => package.replaces = values,
            _ => {}
        }
    }

    if named {
        Some(package)
    } else {
        None
    }
}

/// Parse the `files` file of a local database entry into root-relative paths.
///
/// The file opens with a `%FILES%` header and lists every path the package
/// owns, without a leading slash. Directory entries end in `/` and are
/// dropped: a directory's access time says nothing about whether the package
/// is used.
pub fn parse_files(text: &str) -> Vec<String> {
    let mut files = Vec::new();

    for (key, values) in sections(text) {
        if key != "FILES" {
            continue;
        }
        for value in values {
            if value.ends_with('/') || value.is_empty() {
                continue;
            }
            files.push(value);
        }
    }

    files
}

/// Strip a version constraint from a dependency string.
///
/// `foo>=1.2`, `foo=1.2`, `foo<3` and `libbar.so=1-64` all name `foo` or
/// `libbar.so`. Description text after a colon, used by `%OPTDEPENDS%`, is
/// stripped too.
pub fn dependency_name(spec: &str) -> &str {
    let without_reason = spec.split(':').next().unwrap_or(spec);
    let end = without_reason
        .find(['<', '>', '='])
        .unwrap_or(without_reason.len());
    without_reason.get(..end).unwrap_or(without_reason).trim()
}

/// Split a local database directory name into its package name.
///
/// Entries are named `<pkgname>-<pkgver>-<pkgrel>`. Neither `pkgname` nor
/// `pkgver` may contain a hyphen, so removing the last two hyphen-separated
/// fields recovers the name. Used only as a fallback: `%NAME%` inside `desc`
/// is authoritative.
pub fn name_from_dir(dir_name: &str) -> Option<&str> {
    let (rest, _pkgrel) = dir_name.rsplit_once('-')?;
    let (name, _pkgver) = rest.rsplit_once('-')?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{dependency_name, name_from_dir, parse_desc, parse_files, sections};
    use crate::model::InstallReason;

    const SAMPLE: &str = "%NAME%\nripgrep\n\n%VERSION%\n14.1.1-1\n\n%DESC%\nA search tool\n\n%URL%\nhttps://example.invalid\n\n%SIZE%\n5242880\n\n%INSTALLDATE%\n1700000000\n\n%BUILDDATE%\n1699000000\n\n%REASON%\n1\n\n%GROUPS%\nbase-devel\n\n%DEPENDS%\nglibc\ngcc-libs>=13\n\n%OPTDEPENDS%\nfd: faster walking\n\n%PROVIDES%\nrg=14\n";

    #[test]
    fn sections_group_values_under_keys() {
        let parsed = sections("%A%\none\ntwo\n\n%B%\nthree\n");
        assert_eq!(
            parsed,
            vec![
                ("A".to_owned(), vec!["one".to_owned(), "two".to_owned()]),
                ("B".to_owned(), vec!["three".to_owned()]),
            ]
        );
    }

    #[test]
    fn desc_parses_every_field() {
        let package = parse_desc(SAMPLE, Path::new("/tmp/db")).unwrap();
        assert_eq!(package.name, "ripgrep");
        assert_eq!(package.version, "14.1.1-1");
        assert_eq!(package.description, "A search tool");
        assert_eq!(package.url, "https://example.invalid");
        assert_eq!(package.size, 5_242_880);
        assert_eq!(package.install_date, Some(1_700_000_000));
        assert_eq!(package.build_date, Some(1_699_000_000));
        assert_eq!(package.reason, InstallReason::Dependency);
        assert_eq!(package.groups, vec!["base-devel".to_owned()]);
        assert_eq!(
            package.depends,
            vec!["glibc".to_owned(), "gcc-libs>=13".to_owned()]
        );
        assert_eq!(package.optdepends, vec!["fd: faster walking".to_owned()]);
        assert_eq!(package.provides, vec!["rg=14".to_owned()]);
        assert_eq!(package.db_dir, Path::new("/tmp/db"));
    }

    #[test]
    fn missing_reason_means_explicit() {
        let package = parse_desc("%NAME%\nfoo\n", Path::new("/tmp/db")).unwrap();
        assert_eq!(package.reason, InstallReason::Explicit);
    }

    #[test]
    fn entries_without_a_name_are_rejected() {
        assert!(parse_desc("%VERSION%\n1\n", Path::new("/tmp/db")).is_none());
        assert!(parse_desc("", Path::new("/tmp/db")).is_none());
    }

    #[test]
    fn a_bad_size_does_not_lose_the_package() {
        let package = parse_desc("%NAME%\nfoo\n\n%SIZE%\nnonsense\n", Path::new("/tmp")).unwrap();
        assert_eq!(package.size, 0);
        assert_eq!(package.name, "foo");
    }

    #[test]
    fn files_drop_directories() {
        let files =
            parse_files("%FILES%\nusr/\nusr/bin/\nusr/bin/rg\nusr/share/man/man1/rg.1.gz\n");
        assert_eq!(
            files,
            vec![
                "usr/bin/rg".to_owned(),
                "usr/share/man/man1/rg.1.gz".to_owned()
            ]
        );
    }

    #[test]
    fn dependency_names_drop_constraints() {
        assert_eq!(dependency_name("glibc"), "glibc");
        assert_eq!(dependency_name("gcc-libs>=13"), "gcc-libs");
        assert_eq!(dependency_name("libfoo.so=1-64"), "libfoo.so");
        assert_eq!(dependency_name("python<3.13"), "python");
        assert_eq!(dependency_name("fd: faster walking"), "fd");
    }

    #[test]
    fn directory_names_yield_package_names() {
        assert_eq!(name_from_dir("ripgrep-14.1.1-1"), Some("ripgrep"));
        assert_eq!(name_from_dir("gcc-libs-13.2.1-3"), Some("gcc-libs"));
        assert_eq!(name_from_dir("nope"), None);
    }
}
