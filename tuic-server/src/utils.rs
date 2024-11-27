use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    fs,
    path::Path,
    str::FromStr,
};

use educe::Educe;
use eyre::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};

pub fn load_cert_chain(cert_path: &Path) -> eyre::Result<Vec<CertificateDer<'static>>> {
    let cert_chain = fs::read(cert_path).context("failed to read certificate chain")?;
    let cert_chain = if cert_path.extension().is_some_and(|x| x == "der") {
        vec![CertificateDer::from(cert_chain)]
    } else {
        rustls_pemfile::certs(&mut &*cert_chain)
            .collect::<Result<_, _>>()
            .context("invalid PEM-encoded certificate")?
    };
    Ok(cert_chain)
}

pub fn load_priv_key(key_path: &Path) -> eyre::Result<PrivateKeyDer<'static>> {
    let key = fs::read(key_path).context("failed to read private key")?;
    let key = if key_path.extension().is_some_and(|x| x == "der") {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key))
    } else {
        rustls_pemfile::private_key(&mut &*key)
            .context("malformed PKCS #1 private key")?
            .ok_or_else(|| eyre::Error::msg("no private keys found"))?
    };
    Ok(key)
}

#[derive(Clone, Copy)]
pub enum UdpRelayMode {
    Native,
    Quic,
}

impl Display for UdpRelayMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Native => write!(f, "native"),
            Self::Quic => write!(f, "quic"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Educe)]
#[educe(Default)]
pub enum CongestionController {
    #[educe(Default)]
    Bbr,
    Cubic,
    NewReno,
}

// TODO remove in 2.0.0
impl FromStr for CongestionController {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("cubic") {
            Ok(Self::Cubic)
        } else if s.eq_ignore_ascii_case("new_reno") || s.eq_ignore_ascii_case("newreno") {
            Ok(Self::NewReno)
        } else if s.eq_ignore_ascii_case("bbr") {
            Ok(Self::Bbr)
        } else {
            Err("invalid congestion control")
        }
    }
}

// pub trait ResultExt<T, E> {
//     fn log_err(self) -> Option<T>;
// }
// impl<T> ResultExt<T, anyhow::Report> for Result<T, anyhow::Report>
// {
//     #[inline(always)]
//     fn log_err(self) -> Option<T> {
//         match self {
//             Ok(v) => Some(v),
//             Err(e) => {
//                 tracing::error!("{:?}", e);
//                 None
//             }
//         }
//     }
// }
pub trait FutResultExt<T, E, Fut> {
    async fn log_err(self) -> Option<T>;
}
impl<T, Fut> FutResultExt<T, eyre::Report, Fut> for Fut
where
    Fut: std::future::Future<Output = Result<T, eyre::Report>>,
{
    #[inline(always)]
    async fn log_err(self) -> Option<T> {
        match self.await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!("{:?}", e);
                None
            }
        }
    }
}
