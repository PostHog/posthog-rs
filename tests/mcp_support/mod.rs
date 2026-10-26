//! Shared harness for the MCP analytics tests: an instrumented test server,
//! the events it sends, and ways to call it.

#![allow(dead_code)]

pub mod http;
pub mod recorder;
pub mod server;
