//! Schema compare: options, text comparison and the object-by-object comparison.

pub mod cast;
pub mod compare;
pub mod ddl;
pub mod deploy;
pub mod exists;
pub mod features;
pub mod options;
pub mod print;
pub mod report;
pub mod select;
pub mod text;
pub mod warnings;

pub use compare::{Comparison, DiffKind, Difference, Mappings, ObjectType, PropDiff, TableMapping, compare};
pub use options::{Opt, Options};
