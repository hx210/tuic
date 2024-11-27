use std::{
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    sync::Arc,
};

use eyre::Context;
use quinn::{
    Endpoint, EndpointConfig, IdleTimeout, ServerConfig, TokioRuntime, TransportConfig, VarInt,
    congestion::{BbrConfig, CubicConfig, NewRenoConfig},
    crypto::rustls::QuicServerConfig,
};
use rustls::{
    ServerConfig as RustlsServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tracing::{debug, warn};

use crate::{
    AppContext,
    connection::{Connection, INIT_CONCURRENT_STREAMS},
    error::Error,
    utils::{self, CongestionController},
};

pub struct Server {
    ep: Endpoint,
    ctx: Arc<AppContext>,
}

impl Server {
    pub fn init(ctx: Arc<AppContext>) -> Result<Self, Error> {
        let mut crypto: RustlsServerConfig;
        if ctx.cfg.tls.self_sign {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let cert_der = CertificateDer::from(cert.cert);
            let priv_key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
            crypto = RustlsServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(priv_key))?;
        } else {
            let certs = utils::load_cert_chain(&ctx.cfg.tls.certificate)?;
            let priv_key = utils::load_priv_key(&ctx.cfg.tls.private_key)?;
            crypto = RustlsServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_no_client_auth()
                .with_single_cert(certs, priv_key)?;
        }

        crypto.alpn_protocols = ctx
            .cfg
            .tls
            .alpn
            .iter()
            .cloned()
            .map(|alpn| alpn.into_bytes())
            .collect();
        // TODO only set when 0-RTT enabled
        crypto.max_early_data_size = u32::MAX;
        crypto.send_half_rtt_data = ctx.cfg.zero_rtt_handshake;

        let mut config = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).context("no initial cipher suite found")?,
        ));
        let mut tp_cfg = TransportConfig::default();

        tp_cfg
            .max_concurrent_bidi_streams(VarInt::from(INIT_CONCURRENT_STREAMS))
            .max_concurrent_uni_streams(VarInt::from(INIT_CONCURRENT_STREAMS))
            .send_window(ctx.cfg.quic.send_window)
            .stream_receive_window(VarInt::from_u32(ctx.cfg.quic.receive_window))
            .max_idle_timeout(Some(
                IdleTimeout::try_from(ctx.cfg.quic.max_idle_time)
                    .map_err(|_| Error::InvalidMaxIdleTime)?,
            ))
            .initial_mtu(ctx.cfg.quic.initial_mtu)
            .min_mtu(ctx.cfg.quic.min_mtu)
            .enable_segmentation_offload(ctx.cfg.quic.gso)
            .mtu_discovery_config(if !ctx.cfg.quic.pmtu {
                None
            } else {
                Some(Default::default())
            });

        match ctx.cfg.quic.congestion_control.controller {
            CongestionController::Bbr => {
                let mut bbr_config = BbrConfig::default();
                bbr_config.initial_window(ctx.cfg.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(bbr_config))
            }
            CongestionController::Cubic => {
                let mut cubic_config = CubicConfig::default();
                cubic_config.initial_window(ctx.cfg.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(cubic_config))
            }
            CongestionController::NewReno => {
                let mut new_reno = NewRenoConfig::default();
                new_reno.initial_window(ctx.cfg.quic.congestion_control.initial_window);
                tp_cfg.congestion_controller_factory(Arc::new(new_reno))
            }
        };

        config.transport_config(Arc::new(tp_cfg));

        let socket = {
            let domain = match ctx.cfg.server {
                SocketAddr::V4(_) => Domain::IPV4,
                SocketAddr::V6(_) => Domain::IPV6,
            };

            let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
                .context("failed to create endpoint UDP socket")?;

            if ctx.cfg.dual_stack {
                socket.set_only_v6(!ctx.cfg.dual_stack).map_err(|err| {
                    Error::Socket("endpoint dual-stack socket setting error", err)
                })?;
            }

            socket
                .bind(&SockAddr::from(ctx.cfg.server))
                .context("failed to bind endpoint UDP socket")?;

            StdUdpSocket::from(socket)
        };

        let ep = Endpoint::new(
            EndpointConfig::default(),
            Some(config),
            socket,
            Arc::new(TokioRuntime),
        )?;

        Ok(Self { ep, ctx })
    }

    pub async fn start(&self) {
        warn!(
            "server started, listening on {}",
            self.ep.local_addr().unwrap()
        );
        if self.ctx.cfg.restful.is_some() {
            tokio::spawn(crate::restful::start(self.ctx.clone()));
        }

        loop {
            match self.ep.accept().await {
                Some(conn) => match conn.accept() {
                    Ok(conn) => {
                        tokio::spawn(Connection::handle(self.ctx.clone(), conn));
                    }
                    Err(e) => {
                        debug!("[Incoming] Failed to accept connection: {e}");
                    }
                },
                None => {
                    debug!("[Incoming] the endpoint is closed");
                    return;
                }
            }
        }
    }
}
