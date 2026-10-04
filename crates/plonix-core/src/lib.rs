//! Plonix core engine.
//!
//! The engine is a headless process that owns the intercepting proxy, the
//! traffic store, adaptive scope and the local API. The GUI, the CLI and the
//! MCP server are all clients of the same local API.
//!
//! Each open project is a session with an engine of its own (see
//! [`session`]); the Start screen ([`hub`]) lists, creates and opens them.

pub mod access;
pub mod api;
pub mod ask;
pub mod browser;
pub mod ca;
pub mod crawl;
pub mod codec;
pub mod detect;
pub mod engine;
pub mod exclude;
pub mod insight;
pub mod extension;
pub mod filterpack;
pub mod hub;
pub mod model;
pub mod paths;
pub mod project;
pub mod proxy;
pub mod query;
pub mod registry;
pub mod rulepack;
pub mod scan;
pub mod scope;
pub mod shelf;
pub mod session;
pub mod settings;
pub mod store;
pub mod ui;
pub mod upstream;


pub use engine::{Engine, EngineConfig};
