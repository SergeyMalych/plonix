//! Starts an engine for a test the same way the app and the CLI do: a
//! project opened with `session::open` in a throwaway home.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use plonix_core::Engine;
use plonix_core::paths::Home;
use plonix_core::project::{self, Project};
use plonix_core::session::{self, OpenOptions, Session};
use rustls_pki_types::CertificateDer;

/// An open test project: its engine, listeners and tokens.
pub struct Running {
    pub engine: Arc<Engine>,
    pub proxy_addr: SocketAddr,
    pub api_addr: SocketAddr,
    pub token: String,
    pub agent_token: String,
    pub session: Session,
}

/// Opens a new project called `name` on free ports.
pub async fn open(home: &Home, name: &str, extra_root: Option<CertificateDer<'static>>) -> Running {
    home.ensure().unwrap();
    let p = Project::create(&home.root.join("work").join(project::slug(name)), name).unwrap();
    let options = OpenOptions { proxy_port: Some(0), api_port: Some(0), extra_roots: extra_root.into_iter().collect(), ..Default::default() };
    let session = session::open(home, p, options).await.unwrap();
    Running {
        engine: session.engine.clone(),
        proxy_addr: session.proxy_addr(),
        api_addr: session.api_addr,
        token: session.token.clone(),
        agent_token: home.load_or_create_agent_token().unwrap(),
        session,
    }
}
