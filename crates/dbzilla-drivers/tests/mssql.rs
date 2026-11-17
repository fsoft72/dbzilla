//! SQL Server driver integration tests against a throwaway Docker container.
//! Switched on with `DBZILLA_TEST_MSSQL=1` (the image is big and slow to start).
#![cfg(feature = "mssql")]

mod support;

use std::time::Duration;
use support::Container;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio_util::compat::TokioAsyncWriteCompatExt;

const LABEL_MSSQL: &str = "dbzilla-test-mssql";
const IMAGE: &str = "mcr.microsoft.com/mssql/server:2022-latest";
const SA_PASSWORD: &str = "Dbzilla_Pw1!";
static MSSQL: OnceCell<Option<Container>> = OnceCell::const_new();

#[dtor::dtor(unsafe)]
fn cleanup_containers() {
    support::cleanup(LABEL_MSSQL);
}

/// Opens a raw tiberius client with required TLS and the self-signed certificate trusted.
async fn raw_client(port: u16) -> Result<Client<tokio_util::compat::Compat<TcpStream>>, tiberius::error::Error> {
    let mut cfg = Config::new();
    cfg.host("127.0.0.1");
    cfg.port(port);
    cfg.authentication(AuthMethod::sql_server("sa", SA_PASSWORD));
    cfg.encryption(EncryptionLevel::Required);
    cfg.trust_cert();
    let tcp = TcpStream::connect(cfg.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Client::connect(cfg, tcp.compat_write()).await
}

/// Starts the shared container and waits until a login works. None when the tests are off.
async fn container() -> Option<&'static Container> {
    if !support::mssql_enabled() {
        return None;
    }
    MSSQL
        .get_or_init(|| async {
            let c = support::start(LABEL_MSSQL, IMAGE, &[("ACCEPT_EULA", "Y"), ("MSSQL_SA_PASSWORD", SA_PASSWORD)], 1433, &[])?;
            for _ in 0..240 {
                if raw_client(c.port).await.is_ok() {
                    return Some(c);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            panic!("SQL Server did not become ready");
        })
        .await
        .as_ref()
}

#[tokio::test]
async fn tls_spike_connects() {
    let Some(c) = container().await else { return };
    let mut client = raw_client(c.port).await.expect("login over required TLS");
    let row = client.simple_query("SELECT 1").await.unwrap().into_row().await.unwrap().unwrap();
    assert_eq!(row.get::<i32, _>(0), Some(1));
}
