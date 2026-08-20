//! pacpurge finds space worth reclaiming on an Arch system and helps you take
//! it back, without guessing on your behalf.
//!
//! The crate is split so that everything except [`capability`] is pure:
//!
//! * [`localdb`] parses pacman's on-disk database format;
//! * [`graph`] answers who-needs-what and simulates removal cascades;
//! * [`usage`] turns file access times into a last-used verdict;
//! * [`janitor`] models reclaimable space outside the package set;
//! * [`filter`] sorts and filters the package list for the UI;
//! * [`plan`] turns a selection into the exact commands that will run;
//! * [`scan`] is the one place those pieces meet the filesystem.

#![cfg_attr(
    not(test),
    deny(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)
)]

pub mod app;
pub mod capability;
pub mod cli;
pub mod filter;
pub mod format;
pub mod graph;
pub mod janitor;
pub mod keys;
pub mod localdb;
pub mod model;
pub mod plan;
pub mod report;
pub mod scan;
pub mod ui;
pub mod usage;
