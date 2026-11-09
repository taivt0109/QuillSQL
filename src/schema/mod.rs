//! Schema compare: options, text comparison and the object-by-object comparison.

pub mod backup;
pub mod cast;
pub mod clr;
pub mod compare;
mod compare_objects;
pub mod ddl;
pub mod ddl_objects;
pub mod filter;
pub mod deploy;
pub mod exists;
pub mod features;
pub mod linediff;
pub mod options;
pub mod filter_edit;
pub mod print;
pub mod report;
pub mod select;
pub mod text;
pub mod warnings;
#[cfg(test)]
mod option_tests;

pub use compare::{Comparison, DiffKind, Difference, Mappings, ObjectType, PropDiff, TableMapping, compare};
pub use options::{Opt, Options};
