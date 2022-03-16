use crate::certificate;
use anyhow::{bail, Context, Result};
use getopts::Options;
use rustls::{Certificate, PrivateKey};
use std::time::Duration;

pub struct ConfigBuilder<'cfg> {
    opts: Options,
    program: Option<&'cfg str>,
}

impl<'cfg> ConfigBuilder<'cfg> {
    pub fn new() -> Self {
        let mut opts = Options::new();

        opts.optopt(
            "p",
            "port",
            "Set the listening port(Required)",
            "SERVER_PORT",
        );

        opts.optopt(
            "t",
            "token",
            "Set the TUIC token for the authentication(Required)",
            "TOKEN",
        );

        opts.optopt(
            "c",
            "cert",
            "Set the certificate for QUIC handshake(Required)",
            "CERTIFICATE",
        );

        opts.optopt(
            "k",
            "priv-key",
            "Set the private key for QUIC handshake(Required)",
            "PRIVATE_KEY",
        );

        opts.optopt(
            "",
            "authentication-timeout",
            "Set the maximum time allowed between connection establishment and authentication packet receipt, in milliseconds. Default: 1000ms",
            "AUTHENTICATION_TIMEOUT",
        );

        opts.optopt(
            "",
            "congestion-controller",
            r#"Set the congestion controller. Available: "cubic", "new_reno", "bbr". Default: "cubic""#,
            "CONGESTION_CONTROLLER",
        );

        opts.optflag("v", "version", "Print the version");
        opts.optflag("h", "help", "Print this help menu");

        Self {
            opts,
            program: None,
        }
    }

    pub fn get_usage(&self) -> String {
        self.opts.usage(&format!(
            "Usage: {} [options]",
            self.program.unwrap_or(env!("CARGO_PKG_NAME"))
        ))
    }

    pub fn parse(&mut self, args: &'cfg [String]) -> Result<Config> {
        self.program = Some(&args[0]);

        let matches = self.opts.parse(&args[1..])?;

        if matches.opt_present("h") {
            bail!("{}", self.get_usage());
        }

        if matches.opt_present("v") {
            bail!("{}", env!("CARGO_PKG_VERSION"));
        }

        if !matches.free.is_empty() {
            bail!("Unexpected argument: {}", matches.free.join(", "),);
        }

        let port = matches
            .opt_str("p")
            .context("Required option 'port' missing")?
            .parse()?;

        let token_digest = {
            let token = matches
                .opt_str("t")
                .context("Required option 'token' missing")?;
            *blake3::hash(&token.into_bytes()).as_bytes()
        };

        let certificate = {
            let path = matches
                .opt_str("c")
                .context("Required option 'cert' missing")?;
            certificate::load_certificate(&path)?
        };

        let private_key = {
            let path = matches
                .opt_str("k")
                .context("Required option 'priv-key' missing")?;
            certificate::load_private_key(&path)?
        };

        let authentication_timeout =
            if let Some(duration) = matches.opt_str("authentication-timeout") {
                let duration = duration.parse()?;
                Duration::from_millis(duration)
            } else {
                Duration::from_millis(1000)
            };

        let congestion_controller =
            if let Some(controller) = matches.opt_str("congestion-controller") {
                match controller.as_str() {
                    "cubic" => CongestionController::Cubic,
                    "new_reno" => CongestionController::NewReno,
                    "bbr" => CongestionController::Bbr,
                    _ => bail!("Unknown congestion controller: {}", controller),
                }
            } else {
                CongestionController::Cubic
            };

        Ok(Config {
            port,
            token_digest,
            certificate,
            private_key,
            authentication_timeout,
            congestion_controller,
        })
    }
}

pub struct Config {
    pub port: u16,
    pub token_digest: [u8; 32],
    pub certificate: Certificate,
    pub private_key: PrivateKey,
    pub authentication_timeout: Duration,
    pub congestion_controller: CongestionController,
}

pub enum CongestionController {
    Cubic,
    NewReno,
    Bbr,
}
