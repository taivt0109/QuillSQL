//! Data compare: pairs tables and keys, reads both sides in key order, merges them and stores
//! the differing rows, then scripts the changes.

mod backup_rows;
pub mod compare;
pub mod mapping;
pub mod options;
pub mod report;
pub mod saved;
pub mod store;
pub mod sync;
pub mod value;

pub use options::{DataOpt, DataOptions};
