//! The database schema model and how it is read from SQL Server.

pub mod model;
pub mod read;

pub use model::*;
pub use read::read_database;
