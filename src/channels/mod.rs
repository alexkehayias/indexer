//! Channels: named streams of events that loops and other agents subscribe to.
//!
//! A channel is served by one of three owners ([`ChannelOwner`]): a Unix socket
//! publisher (`Socket`), an in-process server publisher (`Server`, e.g. the
//! default timer channels), or an agent that created it via tools (`Agent`).
//! See [`models`] for the owner type, [`db`] for persistence, [`registry`] for
//! the in-process bus, [`subscribe`] for source-aware subscription, and
//! [`default`] for the built-in timer channels.

pub mod db;
pub mod default;
pub mod models;
pub mod prompt;
pub mod registry;
pub mod subscribe;

pub use models::{Channel, ChannelOwner};
