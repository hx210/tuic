use std::{
    collections::hash_map::Entry,
    io::{Error as IoError, ErrorKind},
    net::SocketAddr,
};

use bytes::Bytes;
use eyre::{OptionExt, eyre};
use tokio::{
    io::{self, AsyncWriteExt},
    net::{self, TcpStream},
};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use tracing::{info, warn};
use tuic::Address;
use tuic_quinn::{Authenticate, Connect, Packet};

use super::{Connection, ERROR_CODE, UdpSession};
use crate::{error::Error, restful, utils::UdpRelayMode};

impl Connection {
    pub async fn handle_authenticate(&self, auth: Authenticate) {
        info!(
            "[{id:#010x}] [{addr}] [{user}] [AUTH] {auth_uuid}",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
            auth_uuid = auth.uuid(),
        );
    }

    pub async fn handle_connect(&self, conn: Connect) {
        let target_addr = conn.addr().to_string();

        info!(
            "[{id:#010x}] [{addr}] [{user}] [TCP] {target_addr}",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
        );

        let process = async {
            let mut stream = None;
            let mut last_err = None;

            match resolve_dns(conn.addr()).await {
                Ok(addrs) => {
                    for addr in addrs {
                        match TcpStream::connect(addr).await {
                            Ok(s) => {
                                s.set_nodelay(true)?;
                                stream = Some(s);
                                break;
                            }
                            Err(err) => last_err = Some(err),
                        }
                    }
                }
                Err(err) => last_err = Some(err),
            }

            if let Some(mut stream) = stream {
                let mut conn = conn.compat();
                let res = io::copy_bidirectional(&mut conn, &mut stream).await;
                _ = conn.get_mut().reset(ERROR_CODE);
                _ = stream.shutdown().await;
                // a -> b tx
                // a <- b rx
                let (tx, rx) = res?;
                let uuid = self.auth.get().unwrap();
                restful::traffic_tx(&self.ctx, &uuid, tx);
                restful::traffic_rx(&self.ctx, &uuid, rx);
                Ok::<_, Error>(())
            } else {
                let _ = conn.compat().shutdown().await;
                Err(last_err
                    .unwrap_or_else(|| IoError::new(ErrorKind::NotFound, "no address resolved")))?
            }
        };

        match process.await {
            Ok(()) => {}
            Err(err) => warn!(
                "[{id:#010x}] [{addr}] [{user}] [TCP] {target_addr}: {err}",
                id = self.id(),
                addr = self.inner.remote_address(),
                user = self.auth,
            ),
        }
    }

    pub async fn handle_packet(&self, pkt: Packet, mode: UdpRelayMode) {
        let assoc_id = pkt.assoc_id();
        let pkt_id = pkt.pkt_id();
        let frag_id = pkt.frag_id();
        let frag_total = pkt.frag_total();

        info!(
            "[{id:#010x}] [{addr}] [{user}] [UDP-OUT] [{assoc_id:#06x}] [from-{mode}] \
             [{pkt_id:#06x}] fragment {frag_id}/{frag_total}",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
            frag_id = frag_id + 1,
        );

        self.udp_relay_mode.store(Some(mode).into());

        let (pkt, addr, assoc_id) = match pkt.accept().await {
            Ok(None) => return,
            Ok(Some(res)) => res,
            Err(err) => {
                warn!(
                    "[{id:#010x}] [{addr}] [{user}] [UDP-OUT] [{assoc_id:#06x}] [from-{mode}] \
                     [{pkt_id:#06x}] fragment {frag_id}/{frag_total}: {err}",
                    id = self.id(),
                    addr = self.inner.remote_address(),
                    user = self.auth,
                    frag_id = frag_id + 1,
                );
                return;
            }
        };

        let process = async {
            info!(
                "[{id:#010x}] [{addr}] [{user}] [UDP-OUT] [{assoc_id:#06x}] [from-{mode}] \
                 [{pkt_id:#06x}] to {src_addr}",
                id = self.id(),
                addr = self.inner.remote_address(),
                user = self.auth,
                src_addr = addr,
            );

            let guard = self.udp_sessions.read().await;
            let session = guard.get(&assoc_id).map(|v| v.to_owned());
            drop(guard);
            let session = match session {
                Some(v) => v,
                None => match self.udp_sessions.write().await.entry(assoc_id) {
                    Entry::Occupied(entry) => entry.get().clone(),
                    Entry::Vacant(entry) => {
                        let session = UdpSession::new(self.ctx.clone(), self.clone(), assoc_id)?;
                        entry.insert(session.clone());
                        session
                    }
                },
            };

            let Some(socket_addr) = resolve_dns(&addr).await?.next() else {
                return Err(Error::from(IoError::new(
                    ErrorKind::NotFound,
                    "no address resolved",
                )));
            };
            restful::traffic_tx(&self.ctx, &self.auth.get().unwrap(), pkt.len() as u64);
            if let Some(session) = session.upgrade() {
                session.send(pkt, socket_addr).await
            } else {
                Err(eyre!("UdpSession dropped already").into())
            }
        };

        if let Err(err) = process.await {
            warn!(
                "[{id:#010x}] [{addr}] [{user}] [UDP-OUT] [{assoc_id:#06x}] [from-{mode}] \
                 [{pkt_id:#06x}] to {src_addr}: {err}",
                id = self.id(),
                addr = self.inner.remote_address(),
                user = self.auth,
                src_addr = addr,
            );
        }
    }

    pub async fn handle_dissociate(&self, assoc_id: u16) {
        info!(
            "[{id:#010x}] [{addr}] [{user}] [UDP-DROP] [{assoc_id:#06x}]",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
        );

        if let Some(session) = self.udp_sessions.write().await.remove(&assoc_id)
            && let Some(session) = session.upgrade()
        {
            session.close().await;
        }
    }

    pub async fn handle_heartbeat(&self) {
        info!(
            "[{id:#010x}] [{addr}] [{user}] [HB]",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
        );
    }

    pub async fn relay_packet(self, pkt: Bytes, addr: Address, assoc_id: u16) -> eyre::Result<()> {
        let addr_display = addr.to_string();

        info!(
            "[{id:#010x}] [{addr}] [{user}] [UDP-IN] [{assoc_id:#06x}] [to-{mode}] from {src_addr}",
            id = self.id(),
            addr = self.inner.remote_address(),
            user = self.auth,
            mode = self.udp_relay_mode.load().unwrap(),
            src_addr = addr_display,
        );

        restful::traffic_rx(
            &self.ctx,
            &self.auth.get().ok_or_eyre("Unreachable")?,
            pkt.len() as u64,
        );

        let res = match self.udp_relay_mode.load().unwrap() {
            UdpRelayMode::Native => self.model.packet_native(pkt, addr, assoc_id),
            UdpRelayMode::Quic => self.model.packet_quic(pkt, addr, assoc_id).await,
        };

        if let Err(err) = res {
            warn!(
                "[{id:#010x}] [{addr}] [{user}] [UDP-IN] [{assoc_id:#06x}] [to-{mode}] from \
                 {src_addr}: {err}",
                id = self.id(),
                addr = self.inner.remote_address(),
                user = self.auth,
                mode = self.udp_relay_mode.load().unwrap(),
                src_addr = addr_display,
            );
        }
        Ok(())
    }
}

async fn resolve_dns(addr: &Address) -> Result<impl Iterator<Item = SocketAddr>, IoError> {
    match addr {
        Address::None => Err(IoError::new(ErrorKind::InvalidInput, "empty address")),
        Address::DomainAddress(domain, port) => Ok(net::lookup_host((domain.as_str(), *port))
            .await?
            .collect::<Vec<_>>()
            .into_iter()),
        Address::SocketAddress(addr) => Ok(vec![*addr].into_iter()),
    }
}
