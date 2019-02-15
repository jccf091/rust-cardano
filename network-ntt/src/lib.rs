//! all the network related actions and processes
//!
//! This module only provides and handle the different connections
//! and act as message passing between the other modules (blockchain,
//! transactions...);

#[macro_use]
extern crate log;
extern crate protocol_tokio as protocol;

pub mod client;
pub mod server;
