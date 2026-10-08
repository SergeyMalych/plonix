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
pub mod apispec;
pub mod ask;
pub mod authcheck;
pub mod assistant;
pub mod browser;
pub mod browser_crawl;
pub mod callbacks;
pub mod ca;
pub mod cdp;
pub mod chats;
pub mod chromium;
pub mod client;
pub mod clientcert;
pub mod crawl;
pub mod codec;
pub mod crash;
pub mod demo;
pub mod detect;
pub mod dialogs;
pub mod engine;
pub mod exclude;
pub mod insight;
pub mod listpack;
pub mod intercept;
pub mod market;
pub mod mcp;
pub mod extension;
pub mod github;
pub mod detectorpack;
pub mod filterpack;
pub mod har;
pub mod hub;
pub mod model;
pub mod paths;
pub mod platform;
pub mod blocklist;
pub mod bounty;
pub mod profile;
pub mod program;
pub mod project;
pub mod proposal;
pub mod proxy;
pub mod query;
pub mod render;
pub mod report;
pub mod registry;
pub mod replace;
pub mod rulepack;
pub mod runs;
pub mod sandbox;
pub mod scan;
pub mod scope;
pub mod shelf;
pub mod skill;
pub mod session;
pub mod settings;
pub mod store;
pub mod terms;
pub mod tool;
pub mod trust;
pub mod ui;
pub mod upstream;
pub mod users;
pub mod watch;
pub mod usage;
pub mod websocket;


pub use engine::Engine;
