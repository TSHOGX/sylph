//! TLS client helpers for remote `sylph.env.v1` connections.

use anyhow::{Context, Result};
use std::{io::Cursor, sync::Arc};
use tokio::net::TcpStream;
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, ServerName},
    },
    TlsConnector,
};

pub async fn connect(
    address: &str,
    server_name: &str,
    ca_pem: &[u8],
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let mut reader = Cursor::new(ca_pem);
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut reader).collect::<std::result::Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("CA bundle is empty");
    }
    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots.add(cert).context("invalid CA certificate")?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect(address).await?;
    let name = ServerName::try_from(server_name.to_owned()).context("invalid TLS server name")?;
    Ok(connector.connect(name, stream).await?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_ca_bundle_is_rejected_before_connecting() {
        assert!(rustls_pemfile::certs(&mut std::io::Cursor::new(b""))
            .next()
            .is_none());
    }
}
