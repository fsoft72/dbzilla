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

/// Makes `ring` the process-wide rustls crypto provider, once. `tiberius` pulls in `aws-lc-rs` too, and with
/// both providers compiled in, rustls no longer picks one by itself and every TLS client built from the
/// default provider (MySQL) would panic.
#[cfg(any(feature = "mysql", feature = "mssql"))]
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // An error means the application already installed a provider, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Returns the driver for an id, or an error when its Cargo feature is not compiled in.
pub fn driver_for(id: DriverId) -> DriverResult<Box<dyn Driver>> {
    #[cfg(any(feature = "mysql", feature = "mssql"))]
    install_crypto_provider();
    match id {
        #[cfg(feature = "sqlite")]
        DriverId::Sqlite => Ok(Box::new(sqlite::SqliteDriver::new())),
        #[cfg(feature = "postgres")]
        DriverId::Postgres => Ok(Box::new(postgres::PostgresDriver::new())),
        #[cfg(feature = "mysql")]
        DriverId::MySql => Ok(Box::new(mysql::MySqlDriver::new())),
        #[cfg(feature = "mssql")]
        DriverId::SqlServer => Ok(Box::new(mssql::MsSqlDriver::new())),
        #[allow(unreachable_patterns)]
        other => Err(DriverError::Unsupported(format!("driver {other:?} is not compiled into this build"))),
    }
}
