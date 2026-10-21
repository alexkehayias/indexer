use serde::{Deserialize, Serialize};

/// Who owns a channel, and therefore how it is served and who may emit.
///
/// Derived from the `channel` row's `source` and `created_by` columns; see
/// [`ChannelOwner::from_columns`]. `webhook` channels are treated as
/// [`ChannelOwner::Server`] since they are published in-process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelOwner {
    /// Server-defined: the default timer channels and webhooks.
    Server,
    /// An external publisher broadcasting over a Unix domain socket.
    Socket,
    /// Created by an agent via tools; carries the creating session/loop id.
    /// Only this agent may emit to the channel.
    Agent(String),
}

impl ChannelOwner {
    /// Reconstruct an owner from a `channel` row's `source` and `created_by`.
    pub fn from_columns(source: &str, created_by: Option<String>) -> Self {
        match created_by {
            Some(id) => Self::Agent(id),
            None if source == "socket" => Self::Socket,
            None => Self::Server,
        }
    }

    /// The `source` column value for this owner. Agent-owned channels are
    /// served in-process, so they record the `server` source.
    pub fn source_str(&self) -> &str {
        match self {
            Self::Socket => "socket",
            Self::Server | Self::Agent(_) => "server",
        }
    }

    /// The creating session id, for [`ChannelOwner::Agent`] channels.
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent(id) => Some(id),
            _ => None,
        }
    }

    /// A short label for display and the channels API: `server`, `socket`, or
    /// `agent`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Server => "server",
            Self::Socket => "socket",
            Self::Agent(_) => "agent",
        }
    }
}

/// An event source that loops can subscribe to.
#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: String,
    pub owner: ChannelOwner,
    /// Debounce window (ms) coalescing a channel's line burst into one event.
    pub debounce_ms: i64,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_columns_maps_agent_first() {
        assert_eq!(
            ChannelOwner::from_columns("server", Some("sess-1".to_string())),
            ChannelOwner::Agent("sess-1".to_string())
        );
        // An agent id wins even for a socket source.
        assert_eq!(
            ChannelOwner::from_columns("socket", Some("sess-1".to_string())),
            ChannelOwner::Agent("sess-1".to_string())
        );
    }

    #[test]
    fn from_columns_distinguishes_socket_and_server() {
        assert_eq!(
            ChannelOwner::from_columns("socket", None),
            ChannelOwner::Socket
        );
        assert_eq!(
            ChannelOwner::from_columns("server", None),
            ChannelOwner::Server
        );
        assert_eq!(
            ChannelOwner::from_columns("webhook", None),
            ChannelOwner::Server
        );
    }

    #[test]
    fn agent_channels_are_server_sourced_and_expose_their_id() {
        let owner = ChannelOwner::Agent("sess-1".to_string());
        assert_eq!(owner.source_str(), "server");
        assert_eq!(owner.agent_id(), Some("sess-1"));
        assert_eq!(owner.as_str(), "agent");
        assert_eq!(ChannelOwner::Socket.agent_id(), None);
        assert_eq!(ChannelOwner::Server.as_str(), "server");
    }

    #[test]
    fn round_trips_through_columns() {
        for owner in [
            ChannelOwner::Server,
            ChannelOwner::Socket,
            ChannelOwner::Agent("sess-1".to_string()),
        ] {
            let restored = ChannelOwner::from_columns(
                owner.source_str(),
                owner.agent_id().map(str::to_string),
            );
            assert_eq!(restored, owner);
        }
    }
}
