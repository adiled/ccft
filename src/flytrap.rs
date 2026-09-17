//! Flytrap lifecycle: load CA, configure hudsucker, run until shutdown signal.

use crate::config::{paths, Config};
use crate::handler::{record_state_on_startup, CcftHandler};
use hudsucker::{
    certificate_authority::RcgenAuthority,
    rcgen::{CertificateParams, DistinguishedName, DnType, Issuer, KeyPair},
    rustls::crypto::aws_lc_rs,
    Proxy,
};
use std::{net::SocketAddr, sync::Arc};
use tracing::*;

pub async fn run(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let (key_pair, ca_cert_pem) = load_or_generate_ca().await?;
    let issuer = Issuer::from_ca_cert_pem(&ca_cert_pem, key_pair)?;
    let ca = RcgenAuthority::new(issuer, 1_000, aws_lc_rs::default_provider());

    let host: std::net::IpAddr = cfg.host.parse().unwrap_or(std::net::IpAddr::V4([127, 0, 0, 1].into()));
    let addr = SocketAddr::new(host, cfg.port);

    info!(
        "ccft listening on {} (pain={} ledger={} override={}chars)",
        addr,
        cfg.pain_enabled,
        cfg.ledger_enabled,
        cfg.system_override.len()
    );
    info!("CA cert: {}", paths::ca_pem().display());

    record_state_on_startup(&cfg);
    let cfg = Arc::new(cfg);
    let handler = CcftHandler::new(Arc::clone(&cfg));

    let flytrap = Proxy::builder()
        .with_addr(addr)
        .with_ca(ca)
        .with_rustls_connector(aws_lc_rs::default_provider())
        .with_http_handler(handler)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("install ctrl-c handler");
        })
        .build()?;

    flytrap.start().await?;
    Ok(())
}

pub async fn load_or_generate_ca() -> Result<(KeyPair, String), Box<dyn std::error::Error>> {
    let dir = paths::ca_dir();
    tokio::fs::create_dir_all(&dir).await?;

    let cert_path = paths::ca_pem();
    let key_path = paths::ca_key();

    if cert_path.exists() && key_path.exists() {
        let cert_pem = tokio::fs::read_to_string(&cert_path).await?;
        let key_pem = tokio::fs::read_to_string(&key_path).await?;
        let key_pair = KeyPair::from_pem(&key_pem)?;
        return Ok((key_pair, cert_pem));
    }

    info!("generating fresh CA at {}", dir.display());
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params.distinguished_name = {
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "ccft CA");
        dn.push(DnType::OrganizationName, "ccft");
        dn
    };
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);

    let cert = params.self_signed(&key_pair)?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    tokio::fs::write(&cert_path, &cert_pem).await?;
    tokio::fs::write(&key_path, &key_pem).await?;

    Ok((key_pair, cert_pem))
}
