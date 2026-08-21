//! Dependency analysis over the installed package set.
//!
//! Two questions matter for deciding what is safe to delete:
//!
//! 1. *Who still needs this?* — a package installed as a dependency that
//!    nothing depends on any more is an orphan, and orphans are the safest
//!    thing on the system to remove.
//! 2. *What does removing it actually free?* — pacman's `-Rns` also takes
//!    dependencies that nothing else keeps alive, so a 40 MiB package can
//!    easily reclaim 400 MiB. Sorting by installed size alone hides that,
//!    which is why this module simulates the cascade instead.
//!
//! Pure: it operates on already-parsed [`Package`] values.

use std::collections::{BTreeMap, BTreeSet};

use crate::localdb::dependency_name;
use crate::model::{InstallReason, Package};

/// Packages that must never be removed casually, beyond the `base` closure.
///
/// The `base` meta-package pulls in glibc, bash, coreutils, pacman and
/// systemd, so the closure below covers most of the system. These names cover
/// what `base` does not: the kernel, the bootloader and the tools needed to
/// recover a system after a bad removal.
const CRITICAL: [&str; 14] = [
    "base",
    "linux",
    "linux-lts",
    "linux-zen",
    "linux-hardened",
    "linux-firmware",
    "mkinitcpio",
    "dracut",
    "grub",
    "systemd-boot",
    "refind",
    "efibootmgr",
    "sudo",
    "pacman",
];

/// A resolved view of the installed package set.
pub struct Graph<'a> {
    packages: &'a [Package],
    /// Package name to index.
    index: BTreeMap<&'a str, usize>,
    /// Hard dependency edges, as indices into `packages`.
    depends: Vec<Vec<usize>>,
    /// Reverse hard dependency edges.
    required_by: Vec<Vec<usize>>,
    /// Reverse optional dependency edges.
    optional_for: Vec<Vec<usize>>,
    /// Packages that must not be removed without an explicit override.
    protected: Vec<bool>,
}

impl<'a> Graph<'a> {
    /// Build the graph, resolving `provides` so that a dependency satisfied by
    /// a virtual name still produces an edge.
    pub fn build(packages: &'a [Package]) -> Graph<'a> {
        let mut index = BTreeMap::new();
        for (position, package) in packages.iter().enumerate() {
            index.insert(package.name.as_str(), position);
        }

        let providers = provider_index(packages, &index);

        let mut depends = vec![Vec::new(); packages.len()];
        let mut required_by = vec![Vec::new(); packages.len()];
        let mut optional_for = vec![Vec::new(); packages.len()];

        for (position, package) in packages.iter().enumerate() {
            for spec in &package.depends {
                for target in resolve(spec, &index, &providers) {
                    if target == position {
                        continue;
                    }
                    push_unique(&mut depends, position, target);
                    push_unique(&mut required_by, target, position);
                }
            }

            for spec in &package.optdepends {
                for target in resolve(spec, &index, &providers) {
                    if target == position {
                        continue;
                    }
                    push_unique(&mut optional_for, target, position);
                }
            }
        }

        let protected = protected_set(packages, &index, &depends);

        Graph {
            packages,
            index,
            depends,
            required_by,
            optional_for,
            protected,
        }
    }

    /// Position of a package by name.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    /// Names of installed packages that hard-depend on `position`.
    pub fn required_by(&self, position: usize) -> Vec<String> {
        self.names(self.required_by.get(position).map(Vec::as_slice))
    }

    /// Names of installed packages listing `position` as an optional dependency.
    pub fn optional_for(&self, position: usize) -> Vec<String> {
        self.names(self.optional_for.get(position).map(Vec::as_slice))
    }

    /// Whether the package needs an explicit override before removal.
    pub fn is_protected(&self, position: usize) -> bool {
        self.protected.get(position).copied().unwrap_or(false)
    }

    /// Simulate `pacman -Rns` over `seed` and return every package that goes.
    ///
    /// A dependency joins the removal set once every package that requires it
    /// is itself being removed. Explicitly installed packages are never swept
    /// up, matching pacman's behaviour, and neither are protected ones.
    pub fn cascade(&self, seed: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut removed: BTreeSet<usize> = seed.clone();
        let mut work: Vec<usize> = Vec::new();

        for position in seed {
            if let Some(children) = self.depends.get(*position) {
                work.extend(children.iter().copied());
            }
        }

        while let Some(candidate) = work.pop() {
            if removed.contains(&candidate) {
                continue;
            }

            let Some(package) = self.packages.get(candidate) else {
                continue;
            };
            if package.reason == InstallReason::Explicit || self.is_protected(candidate) {
                continue;
            }

            let still_needed = self
                .required_by
                .get(candidate)
                .map(|requirers| requirers.iter().any(|requirer| !removed.contains(requirer)))
                .unwrap_or(false);
            if still_needed {
                continue;
            }

            removed.insert(candidate);

            // Anything that was blocked purely by `candidate` is one of its
            // own dependencies, so re-queueing those re-tests exactly the
            // packages this removal may have unblocked.
            if let Some(children) = self.depends.get(candidate) {
                work.extend(children.iter().copied());
            }
        }

        removed
    }

    /// Total installed bytes freed by removing `seed`, cascade included.
    pub fn reclaimable(&self, seed: &BTreeSet<usize>) -> u64 {
        self.cascade(seed)
            .iter()
            .filter_map(|position| self.packages.get(*position))
            .map(|package| package.size)
            .fold(0u64, u64::saturating_add)
    }

    /// Names swept up by removing `position`, excluding `position` itself.
    pub fn dragged_along(&self, position: usize) -> Vec<String> {
        let mut seed = BTreeSet::new();
        seed.insert(position);

        let mut names: Vec<String> = self
            .cascade(&seed)
            .into_iter()
            .filter(|candidate| *candidate != position)
            .filter_map(|candidate| self.packages.get(candidate))
            .map(|package| package.name.clone())
            .collect();
        names.sort();
        names
    }

    /// Packages outside `seed` that hard-depend on something inside it.
    ///
    /// A non-empty result means the removal would break installed software,
    /// and pacman will refuse it without `--cascade`.
    pub fn broken_by(&self, seed: &BTreeSet<usize>) -> Vec<String> {
        let mut broken = BTreeSet::new();

        for position in seed {
            let Some(requirers) = self.required_by.get(*position) else {
                continue;
            };
            for requirer in requirers {
                if seed.contains(requirer) {
                    continue;
                }
                if let Some(package) = self.packages.get(*requirer) {
                    broken.insert(package.name.clone());
                }
            }
        }

        broken.into_iter().collect()
    }

    /// Everything that would have to go with `seed` for the removal to be
    /// consistent: the transitive closure of "depends on something in here".
    ///
    /// This is the answer to *"what else do I have to mark?"*. It is the
    /// closure rather than the direct dependants because marking the direct
    /// ones usually strands their own dependants in turn, and walking that one
    /// layer at a time is a poor way to spend a user's afternoon.
    pub fn dependants(&self, seed: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut reached: BTreeSet<usize> = BTreeSet::new();
        let mut work: Vec<usize> = seed.iter().copied().collect();

        while let Some(position) = work.pop() {
            let Some(requirers) = self.required_by.get(position) else {
                continue;
            };
            for requirer in requirers {
                if seed.contains(requirer) || !reached.insert(*requirer) {
                    continue;
                }
                work.push(*requirer);
            }
        }

        reached
    }

    /// Installed size of a set of packages.
    pub fn total_size(&self, positions: &BTreeSet<usize>) -> u64 {
        positions
            .iter()
            .filter_map(|position| self.packages.get(*position))
            .map(|package| package.size)
            .fold(0u64, u64::saturating_add)
    }

    /// Names for a set of positions, sorted.
    pub fn names_of(&self, positions: &BTreeSet<usize>) -> Vec<String> {
        let mut names: Vec<String> = positions
            .iter()
            .filter_map(|position| self.packages.get(*position))
            .map(|package| package.name.clone())
            .collect();
        names.sort();
        names
    }

    fn names(&self, positions: Option<&[usize]>) -> Vec<String> {
        let mut names: Vec<String> = positions
            .unwrap_or_default()
            .iter()
            .filter_map(|position| self.packages.get(*position))
            .map(|package| package.name.clone())
            .collect();
        names.sort();
        names
    }
}

/// Map every name a package answers to — its own and its `provides` — to the
/// packages offering it.
fn provider_index<'a>(
    packages: &'a [Package],
    index: &BTreeMap<&'a str, usize>,
) -> BTreeMap<&'a str, Vec<usize>> {
    let mut providers: BTreeMap<&'a str, Vec<usize>> = BTreeMap::new();

    for (position, package) in packages.iter().enumerate() {
        for spec in &package.provides {
            let name = dependency_name(spec);
            if name.is_empty() || index.contains_key(name) {
                continue;
            }
            providers.entry(name).or_default().push(position);
        }
    }

    providers
}

/// Resolve a dependency specification to the installed packages satisfying it.
///
/// A real package with the name wins outright. Otherwise every provider of the
/// virtual name is an edge: without pacman's `provides` version comparison
/// this over-approximates, which is the safe direction — it can only make a
/// package look more needed than it is.
fn resolve(
    spec: &str,
    index: &BTreeMap<&str, usize>,
    providers: &BTreeMap<&str, Vec<usize>>,
) -> Vec<usize> {
    let name = dependency_name(spec);
    if name.is_empty() {
        return Vec::new();
    }

    if let Some(position) = index.get(name) {
        return vec![*position];
    }

    providers.get(name).cloned().unwrap_or_default()
}

/// Mark the critical packages and everything they transitively depend on.
fn protected_set(
    packages: &[Package],
    index: &BTreeMap<&str, usize>,
    depends: &[Vec<usize>],
) -> Vec<bool> {
    let mut protected = vec![false; packages.len()];
    let mut work: Vec<usize> = Vec::new();

    for name in CRITICAL {
        if let Some(position) = index.get(name) {
            work.push(*position);
        }
    }

    for (position, package) in packages.iter().enumerate() {
        if package.groups.iter().any(|group| group == "base") {
            work.push(position);
        }
    }

    while let Some(position) = work.pop() {
        match protected.get_mut(position) {
            Some(flag) if *flag => continue,
            Some(flag) => *flag = true,
            None => continue,
        }
        if let Some(children) = depends.get(position) {
            work.extend(children.iter().copied());
        }
    }

    protected
}

/// Append `value` to `slots[slot]` unless it is already there.
fn push_unique(slots: &mut [Vec<usize>], slot: usize, value: usize) {
    if let Some(bucket) = slots.get_mut(slot) {
        if !bucket.contains(&value) {
            bucket.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::Graph;
    use crate::model::{InstallReason, Package};

    fn package(name: &str, size: u64, reason: InstallReason, depends: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            version: "1-1".to_owned(),
            size,
            reason,
            depends: depends.iter().map(|spec| (*spec).to_owned()).collect(),
            ..Package::default()
        }
    }

    /// `app` is explicit and pulls in `lib`, which pulls in `deep`.
    /// `other` is explicit and shares `lib`.
    fn fixture() -> Vec<Package> {
        vec![
            package("app", 100, InstallReason::Explicit, &["lib"]),
            package("lib", 200, InstallReason::Dependency, &["deep>=2"]),
            package("deep", 400, InstallReason::Dependency, &[]),
            package("other", 50, InstallReason::Explicit, &["lib"]),
            package("lonely", 800, InstallReason::Dependency, &[]),
        ]
    }

    #[test]
    fn reverse_edges_resolve_version_constraints() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let deep = graph.position("deep").unwrap();
        assert_eq!(graph.required_by(deep), vec!["lib".to_owned()]);
    }

    #[test]
    fn orphans_have_no_requirers() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let lonely = graph.position("lonely").unwrap();
        assert!(graph.required_by(lonely).is_empty());
        let lib = graph.position("lib").unwrap();
        assert_eq!(
            graph.required_by(lib),
            vec!["app".to_owned(), "other".to_owned()]
        );
    }

    #[test]
    fn a_shared_dependency_survives_a_partial_removal() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("app").unwrap());

        // `other` still needs `lib`, so only `app` goes.
        assert_eq!(graph.reclaimable(&seed), 100);
        assert!(graph
            .dragged_along(graph.position("app").unwrap())
            .is_empty());
    }

    #[test]
    fn removing_the_last_consumer_cascades_transitively() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("app").unwrap());
        seed.insert(graph.position("other").unwrap());

        // app + other + lib + deep, because nothing else keeps lib alive and
        // dropping lib in turn frees deep.
        assert_eq!(graph.reclaimable(&seed), 100 + 50 + 200 + 400);
    }

    #[test]
    fn explicit_packages_are_never_swept_up() {
        let packages = vec![
            package("app", 100, InstallReason::Explicit, &["kept"]),
            package("kept", 900, InstallReason::Explicit, &[]),
        ];
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("app").unwrap());
        assert_eq!(graph.reclaimable(&seed), 100);
    }

    #[test]
    fn provides_satisfies_a_dependency() {
        let mut packages = vec![
            package("client", 10, InstallReason::Explicit, &["mail-server"]),
            package("postfix", 20, InstallReason::Dependency, &[]),
        ];
        if let Some(postfix) = packages.get_mut(1) {
            postfix.provides = vec!["mail-server".to_owned()];
        }

        let graph = Graph::build(&packages);
        let postfix = graph.position("postfix").unwrap();
        assert_eq!(graph.required_by(postfix), vec!["client".to_owned()]);
    }

    #[test]
    fn breakage_is_reported_for_a_still_needed_package() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("lib").unwrap());
        assert_eq!(
            graph.broken_by(&seed),
            vec!["app".to_owned(), "other".to_owned()]
        );
    }

    #[test]
    fn dependants_are_collected_transitively() {
        // start -> mid -> leaf, so removing `leaf` strands both of the others.
        let packages = vec![
            package("leaf", 1, InstallReason::Dependency, &[]),
            package("mid", 2, InstallReason::Dependency, &["leaf"]),
            package("start", 4, InstallReason::Explicit, &["mid"]),
            package("unrelated", 8, InstallReason::Explicit, &[]),
        ];
        let graph = Graph::build(&packages);

        let mut seed = BTreeSet::new();
        seed.insert(graph.position("leaf").unwrap());

        let dependants = graph.dependants(&seed);
        assert_eq!(
            graph.names_of(&dependants),
            vec!["mid".to_owned(), "start".to_owned()]
        );
        assert_eq!(graph.total_size(&dependants), 6);
    }

    #[test]
    fn dependants_of_a_leaf_are_empty() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("app").unwrap());
        assert!(graph.dependants(&seed).is_empty());
    }

    #[test]
    fn dependants_terminate_on_a_cycle() {
        let packages = vec![
            package("a", 1, InstallReason::Dependency, &["b"]),
            package("b", 2, InstallReason::Dependency, &["a"]),
        ];
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("a").unwrap());
        assert_eq!(
            graph.names_of(&graph.dependants(&seed)),
            vec!["b".to_owned()]
        );
    }

    #[test]
    fn dependants_never_include_the_seed() {
        let packages = fixture();
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("lib").unwrap());
        seed.insert(graph.position("app").unwrap());

        let dependants = graph.dependants(&seed);
        assert_eq!(graph.names_of(&dependants), vec!["other".to_owned()]);
    }

    #[test]
    fn the_base_closure_is_protected() {
        let mut packages = vec![
            package("base", 1, InstallReason::Explicit, &["glibc"]),
            package("glibc", 2, InstallReason::Dependency, &[]),
            package("ripgrep", 3, InstallReason::Explicit, &[]),
        ];
        if let Some(base) = packages.get_mut(0) {
            base.groups = vec!["base".to_owned()];
        }

        let graph = Graph::build(&packages);
        assert!(graph.is_protected(graph.position("base").unwrap()));
        assert!(graph.is_protected(graph.position("glibc").unwrap()));
        assert!(!graph.is_protected(graph.position("ripgrep").unwrap()));
    }

    #[test]
    fn a_dependency_cycle_terminates_without_being_swept_up() {
        // `a` and `b` require each other, so each keeps the other alive even
        // once nothing else needs either. pacman's own `-Rns` and `-Qdt` leave
        // cyclic orphans behind for the same reason, and reporting space as
        // reclaimable that pacman will not actually reclaim would be worse
        // than under-reporting it.
        let packages = vec![
            package("a", 1, InstallReason::Dependency, &["b"]),
            package("b", 2, InstallReason::Dependency, &["a"]),
            package("start", 4, InstallReason::Explicit, &["a"]),
        ];
        let graph = Graph::build(&packages);
        let mut seed = BTreeSet::new();
        seed.insert(graph.position("start").unwrap());
        assert_eq!(graph.reclaimable(&seed), 4);
    }
}
