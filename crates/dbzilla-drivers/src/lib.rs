//! Database drivers behind the `Driver` / `Conn` traits.

mod traits;

#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use traits::*;

use dbzilla_core::dialect::DriverId;

/// Returns the driver for an id, or an error when its Cargo feature is not compiled in.
pub fn driver_for(id: DriverId) -> DriverResult<Box<dyn Driver>> {
    match id {
        #[cfg(feature = "sqlite")]
        DriverId::Sqlite => Ok(Box::new(sqlite::SqliteDriver::new())),
        #[allow(unreachable_patterns)]
        other => Err(DriverError::Unsupported(format!("driver {other:?} is not compiled into this build"))),
    }
}
