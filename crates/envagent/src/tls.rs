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

pub async fn connect_stream(
    stream: TcpStream,
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
    // `ServerName::try_from(String)` treats the value as DNS.  IPv6 literals
    // therefore fail even when the certificate is valid for the address. Parse
    // IP literals explicitly so HTTPS pairing/reconnect works on `[::1]` too.
    let name = if let Ok(address) = server_name.parse::<std::net::IpAddr>() {
        ServerName::IpAddress(address.into())
    } else {
        ServerName::try_from(server_name.to_owned()).context("invalid TLS server name")?
    };
    Ok(connector.connect(name, stream).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ca_bundle_is_rejected_before_connecting() {
        assert!(rustls_pemfile::certs(&mut std::io::Cursor::new(b""))
            .next()
            .is_none());
    }

    #[tokio::test]
    async fn configured_ca_and_server_name_complete_a_tls_handshake() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();
        let mut cert_reader = std::io::Cursor::new(cert_pem.as_bytes());
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let mut key_reader = std::io::Cursor::new(key_pem.as_bytes());
        let key = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
            .next()
            .unwrap()
            .unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key.into())
            .unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
        let cert_for_client = cert_pem.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, b"ok")
                .await
                .unwrap();
        });
        let stream = TcpStream::connect(address).await.unwrap();
        let mut tls = connect_stream(stream, "localhost", cert_for_client.as_bytes())
            .await
            .unwrap();
        let mut body = [0u8; 2];
        tokio::io::AsyncReadExt::read_exact(&mut tls, &mut body)
            .await
            .unwrap();
        assert_eq!(&body, b"ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn ipv6_literal_server_name_completes_a_tls_handshake() {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .subject_alt_names
            .push(rcgen::SanType::IpAddress("::1".parse().unwrap()));
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        let mut cert_reader = std::io::Cursor::new(cert_pem.as_bytes());
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let mut key_reader = std::io::Cursor::new(key_pem.as_bytes());
        let key = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
            .next()
            .unwrap()
            .unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key.into())
            .unwrap();
        let listener = match tokio::net::TcpListener::bind(("::1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => return,
            Err(error) => panic!("could not bind IPv6 loopback test listener: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, b"ok")
                .await
                .unwrap();
        });
        let stream = TcpStream::connect(address).await.unwrap();
        let mut tls = connect_stream(stream, "::1", cert_pem.as_bytes())
            .await
            .unwrap();
        let mut body = [0u8; 2];
        tokio::io::AsyncReadExt::read_exact(&mut tls, &mut body)
            .await
            .unwrap();
        assert_eq!(&body, b"ok");
        server.await.unwrap();
    }
}
