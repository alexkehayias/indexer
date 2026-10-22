use serde::{Deserialize, Serialize};

/// How a channel is served — where its events come from.
///
/// Mirrors the `channel.source` column. This is distinct from a channel's
/// *owner*: the nullable `created_by` column naming the agent session that
/// created it, if any (see [`Channel::created_by`]). A channel's source says
/// nothing about who may emit to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelSource {
    /// Served in-process by a server publisher: the default timer channels and
    /// webhooks.
    Server,
    /// An external publisher broadcasting over a Unix domain socket.
    Socket,
}

impl ChannelSource {
    /// Reconstruct a source from a `channel` row's `source` column. Unknown
    /// values (including the reserved `webhook`) are treated as [`Server`],
    /// since they are published in-process.
    ///
    /// [`Server`]: ChannelSource::Server
    pub fn from_column(source: &str) -> Self {
        match source {
            "socket" => Self::Socket,
            _ => Self::Server,
        }
    }

    /// The `source` column value for this source.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Server => "server",
            Self::Socket => "socket",
        }
    }
}

/// An event source that loops can subscribe to.
#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: String,
    /// How the channel is served (`server` or `socket`).
    pub source: ChannelSource,
    /// The agent session that created the channel, if an agent created it via a
    /// tool. `None` for server- and socket-defined channels. Only this owner may
    /// emit to the channel; see [`Channel::is_owned_by`].
    pub created_by: Option<String>,
    /// Debounce window (ms) coalescing a channel's line burst into one event.
    pub debounce_ms: i64,
    pub created_at: String,
}

impl Channel {
    /// Whether `session_id` is this channel's owner (the agent that created it).
    pub fn is_owned_by(&self, session_id: &str) -> bool {
        self.created_by.as_deref() == Some(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_column_distinguishes_socket_and_server() {
        assert_eq!(ChannelSource::from_column("socket"), ChannelSource::Socket);
        assert_eq!(ChannelSource::from_column("server"), ChannelSource::Server);
    }

    #[test]
    fn from_column_treats_webhook_and_unknown_as_server() {
        assert_eq!(ChannelSource::from_column("webhook"), ChannelSource::Server);
        assert_eq!(ChannelSource::from_column(""), ChannelSource::Server);
    }

    #[test]
    fn source_round_trips_through_column() {
        for source in [ChannelSource::Server, ChannelSource::Socket] {
            assert_eq!(ChannelSource::from_column(source.as_str()), source);
        }
    }

    #[test]
    fn is_owned_by_matches_created_by() {
        let owned = Channel {
            id: "c".to_string(),
            source: ChannelSource::Server,
            created_by: Some("sess-1".to_string()),
            debounce_ms: 0,
            created_at: String::new(),
        };
        assert!(owned.is_owned_by("sess-1"));
        assert!(!owned.is_owned_by("sess-2"));

        let unowned = Channel {
            created_by: None,
            ..owned
        };
        assert!(!unowned.is_owned_by("sess-1"));
    }
}
