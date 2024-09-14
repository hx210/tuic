use std::{
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    sync::Arc,
};

use anyhow::Context;
use quinn::{
    congestion::{BbrConfig, CubicConfig, NewRenoConfig},
    crypto::rustls::QuicServerConfig,
    Endpoint, EndpointConfig, IdleTimeout, ServerConfig, TokioRuntime, TransportConfig, VarInt,
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig as RustlsServerConfig,
};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::{
    connection::{Connection, INIT_CONCURRENT_STREAMS},
    error::Error,
    utils::{self, CongestionController},
    CONFIG,
};

pub struct Server {
    ep: Endpoint,
}

impl Server {
    pub fn init() -> Result<Self, Error> {
        let mut crypto: RustlsServerConfig;
        if CONFIG.tls.self_sign {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let cert_der = CertificateDer::from(cert.cert);
            let priv_key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
            crypto = RustlsServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(priv_key))?;
        } else {
            let certs = utils::load_cert_chain(&CONFIG.tls.certificate)?;
            let priv_key = utils::load_priv_key(&CONFIG.tls.private_key)?;
            crypto = RustlsServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_no_client_auth()
                .with_single_cert(certs, priv_key)?;
        }

        crypto.alpn_protocols = CONFIG
            .tls
            .alpn
            .iter()
            .cloned()
            .map(|alpn| alpn.into_bytes())
            .collect();
        // TODO only set when 0-RTT enabled
        crypto.max_early_data_size = u32::MAX;
        crypto.send_half_rtt_data = CONFIG.zero_rtt_handshake;

        let mut config = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).context("no initial cipher suite found")?,
        ));
        let mut tp_cfg = TransportConfig::default();

        tp_cfg
            .max_concurrent_bidi_streams(VarInt::from(INIT_CONCURRENT_STREAMS))
            .max_concurrent_uni_streams(VarInt::from(INIT_CONCURRENT_STREAMS))
            .send_window(CONFIG.quic.send_window)
            .stream_receive_window(VarInt::from_u32(CONFIG.quic.receive_window))
            .max_idle_timeout(Some(
                IdleTimeout::try_from(CONFIG.quic.max_idle_time)
                    .map_err(|_| Error::InvalidMaxIdleTime)?,
            ))
            .initial_mtu(CONFIG.quic.initial_mtu)
            .min_mtu(CONFIG.quic.min_mtu)
            .enable_segmentation_offload(CONFIG.quic.gso)
            .mtu_discovery_config(if !CONFIG.quic.pmtu {
                None
            } else {
                Some(Default::default())
            });

        match CONFIG.quic.congestion_control.controller {
            CongestionController::Bbr => {
                let mut bbr_config = BbrConfig::default();
                bbr_config.initial_window(CONFIG.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(bbr_config))
            }
            CongestionController::Cubic => {
                let mut cubic_config = CubicConfig::default();
                cubic_config.initial_window(CONFIG.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(cubic_config))
            }
            CongestionController::NewReno => {
                let mut new_reno = NewRenoConfig::default();
                new_reno.initial_window(CONFIG.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(new_reno))
            }
        };

        config.transport_config(Arc::new(tp_cfg));

        let socket = {
            let domain = match CONFIG.server {
                SocketAddr::V4(_) => Domain::IPV4,
                SocketAddr::V6(_) => Domain::IPV6,
            };

            let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
                .map_err(|err| Error::Socket("failed to create endpoint UDP socket", err))?;

            if CONFIG.dual_stack {
                socket.set_only_v6(!CONFIG.dual_stack).map_err(|err| {
                    Error::Socket("endpoint dual-stack socket setting error", err)
                })?;
            }

            socket
                .bind(&SockAddr::from(CONFIG.server))
                .map_err(|err| Error::Socket("failed to bind endpoint UDP socket", err))?;

            StdUdpSocket::from(socket)
        };

        let ep = Endpoint::new(
            EndpointConfig::default(),
            Some(config),
            socket,
            Arc::new(TokioRuntime),
        )?;

        Ok(Self { ep })
    }

    pub async fn start(&self) {
        log::warn!(
            "server started, listening on {}",
            self.ep.local_addr().unwrap()
        );
        if CONFIG.restful.is_some() {
            tokio::spawn(crate::restful::start());
        }

        loop {
            match self.ep.accept().await {
                Some(conn) => match conn.accept() {
                    Ok(conn) => {
                        tokio::spawn(Connection::handle(conn));
                    }
                    Err(e) => {
                        log::debug!("[Incoming] Failed to accept connection: {e}");
                    }
                },
                None => {
                    log::debug!("[Incoming] ep.accept() returned None.");
                    return;
                }
            }
        }
    }
}
