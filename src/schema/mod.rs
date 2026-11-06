//! Schema compare: options, text comparison and the object-by-object comparison.

pub mod compare;
pub mod options;
pub mod print;
pub mod select;
pub mod text;

pub use compare::{Comparison, DiffKind, Difference, Mappings, ObjectType, PropDiff, TableMapping, compare};
pub use options::{Opt, Options};
