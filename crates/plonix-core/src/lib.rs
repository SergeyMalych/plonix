//! Plonix core engine.
//!
//! The engine is a headless process that owns the intercepting proxy, the
//! traffic store, adaptive scope and the local API. The GUI, the CLI and the
//! MCP server are all clients of the same local API.

pub mod api;
pub mod browser;
pub mod ca;
pub mod codec;
pub mod detect;
pub mod engine;
pub mod extension;
pub mod model;
pub mod paths;
pub mod proxy;
pub mod query;
pub mod registry;
pub mod rulepack;
pub mod scope;
pub mod store;
pub mod ui;
pub mod upstream;


pub use engine::{Engine, EngineConfig};
