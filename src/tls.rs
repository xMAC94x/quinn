use rustls::quic::{ClientQuicExt, ServerQuicExt};
use rustls::{ClientConfig, KeyLogFile, NoClientAuth, ProtocolVersion, TLSError};

use std::io::Cursor;
use std::sync::Arc;

use super::{QuicError, QuicResult};
use codec::Codec;
use crypto::Secret;
use parameters::{ClientTransportParameters, ServerTransportParameters};
use types::Side;

use webpki::{DNSNameRef, TLSServerTrustAnchors};
use webpki_roots;

pub use rustls::{Certificate, ClientSession, PrivateKey, ServerConfig, ServerSession, Session};

pub fn client_session(
    config: Option<ClientConfig>,
    hostname: &str,
    params: &ClientTransportParameters,
) -> QuicResult<ClientSession> {
    let pki_server_name = DNSNameRef::try_from_ascii_str(hostname)
        .map_err(|_| QuicError::InvalidDnsName(hostname.into()))?;
    Ok(ClientSession::new_quic(
        &Arc::new(config.unwrap_or_else(|| build_client_config(None))),
        pki_server_name,
        to_vec(params),
    ))
}

pub fn build_client_config(anchors: Option<&TLSServerTrustAnchors>) -> ClientConfig {
    let mut config = ClientConfig::new();
    let anchors = anchors.unwrap_or(&webpki_roots::TLS_SERVER_ROOTS);
    config.root_store.add_server_trust_anchors(anchors);
    config.versions = vec![ProtocolVersion::TLSv1_3];
    config.alpn_protocols = vec![ALPN_PROTOCOL.into()];
    config.key_log = Arc::new(KeyLogFile::new());
    config
}

pub fn server_session(
    config: &Arc<ServerConfig>,
    params: &ServerTransportParameters,
) -> ServerSession {
    ServerSession::new_quic(config, to_vec(params))
}

pub fn build_server_config(cert_chain: Vec<Certificate>, key: PrivateKey) -> ServerConfig {
    let mut config = ServerConfig::new(NoClientAuth::new());
    config.set_protocols(&[ALPN_PROTOCOL.into()]);
    config.set_single_cert(cert_chain, key);
    config.key_log = Arc::new(KeyLogFile::new());
    config
}

pub fn process_handshake_messages<T>(session: &mut T, msgs: Option<&[u8]>) -> QuicResult<TlsResult>
where
    T: Session,
{
    if let Some(data) = msgs {
        let mut read = Cursor::new(data);
        let did_read = session.read_tls(&mut read)?;
        debug_assert_eq!(did_read, data.len());
        session.process_new_packets()?;
    }

    let key_ready = if !session.is_handshaking() {
        Some(session
            .get_negotiated_ciphersuite()
            .ok_or(TLSError::HandshakeNotComplete)?)
    } else {
        None
    };

    let mut messages = Vec::new();
    session.write_tls(&mut messages)?;

    let secret = if let Some(suite) = key_ready {
        let mut client_secret = vec![0u8; suite.enc_key_len];
        session.export_keying_material(&mut client_secret, b"EXPORTER-QUIC client 1rtt", None)?;
        let mut server_secret = vec![0u8; suite.enc_key_len];
        session.export_keying_material(&mut server_secret, b"EXPORTER-QUIC server 1rtt", None)?;

        let (aead_alg, hash_alg) = (suite.get_aead_alg(), suite.get_hash());
        Some(Secret::For1Rtt(
            aead_alg,
            hash_alg,
            client_secret,
            server_secret,
        ))
    } else {
        None
    };

    Ok((messages, secret))
}

pub trait QuicSide {
    fn side(&self) -> Side;
}

impl QuicSide for ClientSession {
    fn side(&self) -> Side {
        Side::Client
    }
}

impl QuicSide for ServerSession {
    fn side(&self) -> Side {
        Side::Server
    }
}

type TlsResult = (Vec<u8>, Option<Secret>);

fn to_vec<T: Codec>(val: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    val.encode(&mut bytes);
    bytes
}

const ALPN_PROTOCOL: &str = "hq-11";
