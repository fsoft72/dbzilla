//! Database drivers behind the `Driver` / `Conn` traits.

mod traits;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "postgres")]
mod pg_value;
#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "mysql")]
pub mod mysql;

#[cfg(feature = "mssql")]
pub mod mssql;

pub use traits::*;

use dbzilla_core::dialect::DriverId;

/// Returns the driver for an id, or an error when its Cargo feature is not compiled in.
pub fn driver_for(id: DriverId) -> DriverResult<Box<dyn Driver>> {
    match id {
        #[cfg(feature = "sqlite")]
        DriverId::Sqlite => Ok(Box::new(sqlite::SqliteDriver::new())),
        #[cfg(feature = "postgres")]
        DriverId::Postgres => Ok(Box::new(postgres::PostgresDriver::new())),
        #[cfg(feature = "mysql")]
        DriverId::MySql => Ok(Box::new(mysql::MySqlDriver::new())),
        #[allow(unreachable_patterns)]
        other => Err(DriverError::Unsupported(format!("driver {other:?} is not compiled into this build"))),
    }
}
