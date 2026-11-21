// ABOUTME: Gates the explicit fixed PCM profile inside the sole Network worker.
// ABOUTME: Tracks ordered role activation without claiming source or output readiness.
use super::DriverError;
use earph_sendspin::{
    message::Complete,
    player::{MAX_CHUNK_WIRE_BYTES, PlayerEvent, PlayerState, PlayerSupport, StreamFormat},
    session::ApplicationMessage,
    timing::UsableMapping,
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerObservation {
    pub activation_generation: u64,
    pub participating: bool,
    pub state_sent: bool,
    /// Engine's declaration, after its independent source, timing, and output checks.
    pub available: bool,
}
#[derive(Clone, Copy)]
pub(super) enum Profile {
    Core,
    Player(PlayerSupport),
}
impl Profile {
    pub fn hello(self, hello: &mut Value) -> Result<(), DriverError> {
        if !hello
            .get("supported_roles")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
            || hello
                .as_object()
                .is_some_and(|h| h.keys().any(|key| key.ends_with("_support")))
        {
            return Err(DriverError::Admission);
        }
        if let Self::Player(support) = self {
            hello["supported_roles"] = json!(["player@v1"]);
            hello["player@v1_support"] = support.to_value();
        }
        Ok(())
    }
    pub fn permits(self, roles: &[String], activities: &[String]) -> bool {
        if roles.is_empty() && activities.len() <= 1 {
            return activities.is_empty()
                || activities[0] == "pairing"
                || (matches!(self, Self::Player(_)) && activities[0] == "playback");
        }
        matches!(self, Self::Player(_))
            && roles == ["player@v1"]
            && (activities.is_empty() || activities == ["playback"])
    }
}
#[derive(Clone, Copy, Debug)]
pub(super) struct StateUpdate {
    pub generation: u64,
    pub mapping: UsableMapping,
    pub state: PlayerState,
    pub available: bool,
}
/// Inline metadata is bounded by the same slots as queued and consumer-held event leases.
#[derive(Clone, Copy)]
pub(super) enum PlayerMetadata {
    Start {
        server_transmitted_us: i64,
        format: StreamFormat,
    },
    Audio,
    Clear {
        server_transmitted_us: i64,
        from_timestamp_us: Option<i64>,
    },
    End,
}
impl From<PlayerEvent<'_>> for PlayerMetadata {
    fn from(event: PlayerEvent<'_>) -> Self {
        match event {
            PlayerEvent::Start {
                server_transmitted_us,
                format,
                ..
            } => Self::Start {
                server_transmitted_us,
                format,
            },
            PlayerEvent::Audio { .. } => Self::Audio,
            PlayerEvent::Clear {
                server_transmitted_us,
                from_timestamp_us,
                ..
            } => Self::Clear {
                server_transmitted_us,
                from_timestamp_us,
            },
            PlayerEvent::End { .. } => Self::End,
        }
    }
}
impl PlayerMetadata {
    pub fn borrow(self, message: &Complete) -> PlayerEvent<'_> {
        let arrival = message.arrival;
        match self {
            Self::Start {
                server_transmitted_us,
                format,
            } => PlayerEvent::Start {
                server_transmitted_us,
                format,
                arrival,
            },
            Self::Audio => PlayerEvent::Audio {
                frame: earph_sendspin::decode_player_audio_body(
                    message.id,
                    &message.body,
                    MAX_CHUNK_WIRE_BYTES,
                )
                .expect("Network validated immutable player header"),
                arrival,
            },
            Self::Clear {
                server_transmitted_us,
                from_timestamp_us,
            } => PlayerEvent::Clear {
                server_transmitted_us,
                from_timestamp_us,
                arrival,
            },
            Self::End => PlayerEvent::End { arrival },
        }
    }
}
/// Reserve queue storage plus the largest simultaneous consumer-held metadata population.
pub(super) fn metadata_capacity_bytes(queue_capacity: usize, leases: usize) -> Option<usize> {
    queue_capacity
        .checked_add(leases)?
        .checked_mul(std::mem::size_of::<Option<PlayerMetadata>>())
}
impl StateUpdate {
    pub fn message(self) -> ApplicationMessage {
        let mut player = self.state.to_value();
        player["available"] = json!(self.available);
        ApplicationMessage::json("client/state", json!({"player":player}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_constructs_only_the_fixed_typed_player_offer() {
        let mut hello = json!({"name":"fixture","supported_roles":[]});
        Profile::Player(PlayerSupport::new(1024).unwrap())
            .hello(&mut hello)
            .unwrap();
        assert_eq!(
            hello,
            json!({"name":"fixture","supported_roles":["player@v1"],"player@v1_support":{"buffer_capacity":1024,"supported_formats":[{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16}]}})
        );
        assert_eq!(Profile::Core.hello(&mut hello), Err(DriverError::Admission));
    }
    #[test]
    fn player_profile_admits_playback_removal_and_rejects_unoffered_mixed_activity() {
        let profile = Profile::Player(PlayerSupport::new(1024).unwrap());
        assert!(profile.permits(&["player@v1".into()], &[]));
        assert!(profile.permits(&[], &["playback".into()]));
        assert!(!profile.permits(
            &["player@v1".into()],
            &["playback".into(), "pairing".into()]
        ));
        assert!(!profile.permits(&["controller@v1".into()], &["playback".into()]));
        assert!(!Profile::Core.permits(&["player@v1".into()], &[]));
    }
}
