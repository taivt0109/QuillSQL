//! Comparison sources besides live databases: scripts folders, source control and the classic compare tool
//! snapshots.

pub mod backup;
pub mod datafolder;
pub mod expr;
pub mod events;
pub mod folder;
mod objects;
pub mod parse;
pub mod scm;
pub mod snp;
pub mod snp_write;
pub mod sqlproj;
pub mod script;

#[cfg(test)]
mod followup_tests;
