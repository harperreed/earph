// ABOUTME: Defines canonical renderer event fields and exact bounded XML encoding.
// ABOUTME: Shares projection and double-escape sizing between Core admission and transport consumers.
use crate::{EngineError, ErrorCode, ids::*, source::StagedUriDescriptor};
use serde::{Deserialize, Serialize};
use std::{
    fmt::{self, Write},
    net::IpAddr,
    sync::Arc,
};

pub const MAX_RENDERER_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_RENDERER_SUBSCRIPTIONS: usize = 16;
pub const MAX_RENDERER_DELIVERIES: usize = 4;
pub const RENDERER_DELIVERY_NS: u64 = 30_000_000_000;
pub const RENDERER_RESPONSE_NS: u64 = 15_000_000_000;
pub const RENDERER_MODERATION_NS: u64 = 5_000_000_000;
pub(crate) const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid renderer event authority");
pub(crate) const PRECONDITION: EngineError = EngineError::new(
    ErrorCode::StaleSession,
    "renderer subscription precondition failed",
);
pub(crate) const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "renderer event admission exceeds limits",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererService {
    AvTransport,
    RenderingControl,
    ConnectionManager,
}
impl RendererService {
    /// Fixed final-buffer slots. The small bounds follow the field table below.
    pub const fn event_slot_bytes(self) -> usize {
        match self {
            Self::AvTransport => MAX_RENDERER_EVENT_BYTES,
            Self::RenderingControl => 2048,
            Self::ConnectionManager => 1024,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererCallbackOrigin {
    pub origin: String,
    pub pins: Vec<IpAddr>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererCallbackGrantDocument {
    pub origins: Vec<RendererCallbackOrigin>,
}
impl fmt::Debug for RendererCallbackGrantDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RendererCallbackGrantDocument([redacted])")
    }
}
impl fmt::Debug for RendererCallbackOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RendererCallbackOrigin([redacted])")
    }
}
impl RendererCallbackGrantDocument {
    pub fn retained_bytes(&self) -> Result<usize, EngineError> {
        if self.origins.capacity() > 8 {
            return Err(BUDGET);
        }
        let mut bytes =
            size_of::<Self>() + 16 + self.origins.capacity() * size_of::<RendererCallbackOrigin>();
        for origin in &self.origins {
            if origin.origin.capacity() > 8192 || origin.pins.capacity() > 8 {
                return Err(BUDGET);
            }
            bytes = bytes
                .checked_add(
                    origin.origin.capacity() + origin.pins.capacity() * size_of::<IpAddr>(),
                )
                .ok_or(BUDGET)?;
        }
        Ok(bytes)
    }
    pub(crate) fn validate(&self, peers: &[IpAddr]) -> Result<(), EngineError> {
        self.retained_bytes()?;
        if self.origins.is_empty() {
            return Err(INVALID);
        }
        for (i, origin) in self.origins.iter().enumerate() {
            let parsed = callback_origin(&origin.origin)?;
            if !parsed.explicit_port
                || parsed.authority_end != origin.origin.len()
                || !origin.origin.starts_with("http://")
                || origin.pins.is_empty()
                || self.origins[..i].iter().any(|o| o.origin == origin.origin)
                || origin.pins.iter().enumerate().any(|(j, p)| {
                    !callback_address_allowed(*p)
                        || !peers.contains(p)
                        || origin.pins[..j].contains(p)
                })
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}
/// Validates the explicit HTTP origin without resolving any hostname.
/// The adapter must also apply its shared URL policy before transport construction.
#[derive(Clone, Copy)]
pub struct RendererCallbackUrlOrigin<'a> {
    pub host: &'a str,
    pub port: u16,
    pub explicit_port: bool,
    pub authority_end: usize,
}
impl fmt::Debug for RendererCallbackUrlOrigin<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RendererCallbackUrlOrigin([redacted])")
    }
}
impl PartialEq for RendererCallbackUrlOrigin<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.host.eq_ignore_ascii_case(other.host) && self.port == other.port
    }
}
impl Eq for RendererCallbackUrlOrigin<'_> {}
pub fn callback_origin(url: &str) -> Result<RendererCallbackUrlOrigin<'_>, EngineError> {
    if url.is_empty()
        || url.len() > 8192
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
        || url.contains(['#', '\\', '<', '>'])
    {
        return Err(INVALID);
    }
    if !url
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        return Err(INVALID);
    }
    let rest = &url[7..];
    let authority = rest.split(['/', '?']).next().ok_or(INVALID)?;
    let (host, port_text) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(INVALID)?;
        let host = &authority[1..end];
        host.parse::<std::net::Ipv6Addr>().map_err(|_| INVALID)?;
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(INVALID)?)
        };
        (host, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if host
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
            || host.starts_with('.')
            || host.ends_with('.')
        {
            return Err(INVALID);
        }
        (host, port)
    };
    if host.is_empty() {
        return Err(INVALID);
    }
    let port = match port_text {
        Some(text) => text
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(INVALID)?,
        None => 80,
    };
    Ok(RendererCallbackUrlOrigin {
        host,
        port,
        explicit_port: port_text.is_some(),
        authority_end: 7 + authority.len(),
    })
}
pub fn callback_address_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && a != 0
                && a < 224
                && !(a == 169 && b == 254)
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && (ip.segments()[0] & 0xffc0) != 0xfe80
                && ip.to_ipv4_mapped().is_none()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererEventSnapshot {
    pub controller_id: RendererControllerId,
    pub controller_generation: u64,
    pub projection: RendererProjection,
}
impl std::ops::Deref for RendererEventSnapshot {
    type Target = RendererProjection;
    fn deref(&self) -> &Self::Target {
        &self.projection
    }
}
#[derive(Clone)]
pub struct RendererProjection {
    pub service: RendererService,
    pub transport_state: &'static str,
    pub transport_status: &'static str,
    pub transport_actions: &'static str,
    pub presets: &'static str,
    pub duration_seconds: Option<u64>,
    pub position_seconds: Option<u64>,
    pub muted: bool,
    pub volume: crate::remote_volume::RemoteVolume,
    pub connected: bool,
    pub descriptor: Option<Arc<StagedUriDescriptor>>,
}
impl fmt::Debug for RendererProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RendererProjection")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}
impl PartialEq for RendererProjection {
    fn eq(&self, other: &Self) -> bool {
        let descriptor_equal = match (&self.descriptor, &other.descriptor) {
            (None, None) => true,
            (Some(a), Some(b)) => a.uri() == b.uri() && a.metadata() == b.metadata(),
            _ => false,
        };
        self.service == other.service
            && match self.service {
                RendererService::AvTransport => {
                    self.transport_state == other.transport_state
                        && self.transport_status == other.transport_status
                        && self.transport_actions == other.transport_actions
                        && self.duration_seconds == other.duration_seconds
                        && descriptor_equal
                }
                RendererService::RenderingControl => {
                    self.muted == other.muted
                        && self.volume == other.volume
                        && self.presets == other.presets
                }
                RendererService::ConnectionManager => self.connected == other.connected,
            }
    }
}
impl Eq for RendererProjection {}

#[derive(Clone, Copy)]
pub enum RendererFieldValue<'a> {
    Text(&'a str),
    Unsigned(u64),
    Signed(i64),
    Bool(bool),
    Time(Option<u64>),
}
impl fmt::Debug for RendererFieldValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RendererFieldValue")
            .field("kind", &std::mem::discriminant(self))
            .finish_non_exhaustive()
    }
}
impl fmt::Display for RendererFieldValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(s) => f.write_str(s),
            Self::Unsigned(n) => write!(f, "{n}"),
            Self::Signed(n) => write!(f, "{n}"),
            Self::Bool(b) => f.write_str(if *b { "1" } else { "0" }),
            Self::Time(None) => f.write_str("NOT_IMPLEMENTED"),
            Self::Time(Some(seconds)) => write!(
                f,
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            ),
        }
    }
}
impl RendererProjection {
    /// The same state-variable table serves SOAP getters and complete event snapshots.
    pub fn fields(&self, field: impl FnMut(&'static str, RendererFieldValue<'_>)) {
        self.visit_fields(true, self.media_fields(), field);
    }
    fn visit_fields(
        &self,
        getters: bool,
        media: Option<(&str, &str)>,
        mut field: impl FnMut(&'static str, RendererFieldValue<'_>),
    ) {
        use RendererFieldValue::*;
        let (uri, metadata) = media.unwrap_or(("", ""));
        match self.service {
            RendererService::AvTransport => {
                for (name, value) in [
                    ("TransportState", self.transport_state),
                    ("TransportStatus", self.transport_status),
                    ("TransportPlaySpeed", "1"),
                    ("AVTransportURI", uri),
                    ("AVTransportURIMetaData", metadata),
                    ("CurrentTrackURI", uri),
                    ("CurrentTrackMetaData", metadata),
                    ("NextAVTransportURI", "NOT_IMPLEMENTED"),
                    ("NextAVTransportURIMetaData", "NOT_IMPLEMENTED"),
                    ("CurrentPlayMode", "NORMAL"),
                    (
                        "PlaybackStorageMedium",
                        if media.is_some() { "NETWORK" } else { "NONE" },
                    ),
                    ("RecordStorageMedium", "NOT_IMPLEMENTED"),
                    ("RecordMediumWriteStatus", "NOT_IMPLEMENTED"),
                    ("CurrentRecordQualityMode", "NOT_IMPLEMENTED"),
                    ("CurrentTransportActions", self.transport_actions),
                ] {
                    field(name, Text(value));
                }
                let count = u64::from(media.is_some());
                field("NumberOfTracks", Unsigned(count));
                field("CurrentTrack", Unsigned(count));
                field(
                    "CurrentMediaDuration",
                    Time(if media.is_some() {
                        self.duration_seconds
                    } else {
                        Some(0)
                    }),
                );
                field("CurrentTrackDuration", Time(self.duration_seconds));
                if getters {
                    field("RelativeTimePosition", Time(self.position_seconds));
                    field("AbsoluteTimePosition", Time(self.position_seconds));
                    field("RelativeCounterPosition", Signed(i32::MAX.into()));
                    field("AbsoluteCounterPosition", Signed(i32::MAX.into()));
                    field("PossiblePlaybackStorageMedia", Text("NETWORK"));
                    field("PossibleRecordStorageMedia", Text("NOT_IMPLEMENTED"));
                    field("PossibleRecordQualityModes", Text("NOT_IMPLEMENTED"));
                }
            }
            RendererService::RenderingControl => {
                field("PresetNameList", Text(self.presets));
                field("Mute", Bool(self.muted));
                field("Volume", Unsigned(self.volume.volume().into()));
                field("VolumeDB", Signed(self.volume.volume_db().into()));
            }
            RendererService::ConnectionManager => {
                field("SourceProtocolInfo", Text(""));
                field("SinkProtocolInfo", Text("http-get:*:audio/flac:*"));
                field(
                    "CurrentConnectionIDs",
                    Text(if self.connected { "0" } else { "" }),
                );
                if getters {
                    field("A_ARG_TYPE_RcsID", Unsigned(0));
                    field("A_ARG_TYPE_AVTransportID", Unsigned(0));
                    field("A_ARG_TYPE_ProtocolInfo", Text("http-get:*:audio/flac:*"));
                    field("A_ARG_TYPE_ConnectionManager", Text(""));
                    field("A_ARG_TYPE_ConnectionID", Signed(-1));
                    field("A_ARG_TYPE_Direction", Text("Input"));
                    field("A_ARG_TYPE_ConnectionStatus", Text("OK"));
                }
            }
        }
    }
    pub fn encoded_size(&self) -> Result<usize, EngineError> {
        let mut counter = SizeWriter(0);
        self.write_xml(&mut counter).map_err(|_| BUDGET)?;
        if counter.0 > self.service.event_slot_bytes() {
            return Err(BUDGET);
        }
        Ok(counter.0)
    }
    /// Writes directly into the adapter's final reserved slot, without an intermediate LastChange.
    pub fn encode_into(&self, buffer: &mut String) -> Result<(), EngineError> {
        let size = self.encoded_size()?;
        if buffer.capacity() < size {
            return Err(BUDGET);
        }
        buffer.clear();
        self.write_xml(buffer).map_err(|_| BUDGET)?;
        Ok(())
    }
    fn media_fields(&self) -> Option<(&str, &str)> {
        self.descriptor.as_ref().map(|d| (d.uri(), d.metadata()))
    }
    fn write_xml(&self, out: &mut impl Write) -> fmt::Result {
        self.write_xml_with_media(out, self.media_fields())
    }
    fn write_xml_with_media(
        &self,
        out: &mut impl Write,
        media: Option<(&str, &str)>,
    ) -> fmt::Result {
        out.write_str("<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">")?;
        let last_change = self.service != RendererService::ConnectionManager;
        if last_change {
            out.write_str(
                "<e:property><LastChange>&lt;Event xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/",
            )?;
            out.write_str(if self.service == RendererService::AvTransport {
                "AVT/"
            } else {
                "RCS/"
            })?;
            out.write_str("&quot;&gt;&lt;InstanceID val=&quot;0&quot;&gt;")?;
        }
        let mut result = Ok(());
        self.visit_fields(false, media, |name, value| {
            if result.is_err() {
                return;
            }
            result = (|| {
                if last_change {
                    write!(out, "&lt;{name}")?;
                    if self.service == RendererService::RenderingControl && name != "PresetNameList"
                    {
                        out.write_str(" channel=&quot;Master&quot;")?;
                    }
                    out.write_str(" val=&quot;")?;
                    write!(
                        EscapedWriter {
                            inner: out,
                            layers: 2
                        },
                        "{value}"
                    )?;
                    out.write_str("&quot;/&gt;")
                } else {
                    write!(out, "<e:property><{name}>")?;
                    write!(
                        EscapedWriter {
                            inner: out,
                            layers: 1
                        },
                        "{value}"
                    )?;
                    write!(out, "</{name}></e:property>")
                }
            })();
        });
        result?;
        if last_change {
            out.write_str("&lt;/InstanceID&gt;&lt;/Event&gt;</LastChange></e:property>")?;
        }
        out.write_str("</e:propertyset>")
    }
}
struct SizeWriter(usize);
impl Write for SizeWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}
struct EscapedWriter<'a, W> {
    inner: &'a mut W,
    layers: u8,
}
impl<W: Write> Write for EscapedWriter<'_, W> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for c in text.chars() {
            if !xml_character(c) {
                return Err(fmt::Error);
            }
            let escaped = match c {
                '\t' if self.layers == 2 => Some("&#x9;"),
                '\r' if self.layers == 2 => Some("&#xD;"),
                '\n' if self.layers == 2 => Some("&#xA;"),
                '&' => Some("&amp;"),
                '<' => Some("&lt;"),
                '>' => Some("&gt;"),
                '"' => Some("&quot;"),
                '\'' => Some("&apos;"),
                _ => None,
            };
            if let Some(escaped) = escaped {
                if self.layers == 2 {
                    self.inner.write_str("&amp;")?;
                    self.inner.write_str(&escaped[1..])?;
                } else {
                    self.inner.write_str(escaped)?;
                }
            } else {
                self.inner.write_char(c)?;
            }
        }
        Ok(())
    }
}

/// Checks the largest complete AVT scalar state before a descriptor becomes canonical.
pub(crate) fn validate_staged_event(uri: &str, metadata: &str) -> Result<(), EngineError> {
    if uri
        .chars()
        .chain(metadata.chars())
        .any(|c| !xml_character(c))
    {
        return Err(INVALID);
    }
    // The descriptor is not allocated here. Count the field table with borrowed candidate text.
    let base = RendererProjection {
        service: RendererService::AvTransport,
        transport_state: "PAUSED_PLAYBACK",
        transport_status: "ERROR_OCCURRED",
        transport_actions: "Play,Stop,Pause",
        presets: "FactoryDefaults",
        duration_seconds: Some(u64::MAX),
        position_seconds: None,
        muted: true,
        volume: crate::remote_volume::RemoteVolume::from_volume(0)?,
        connected: true,
        descriptor: None,
    };
    let mut counter = SizeWriter(0);
    base.write_xml_with_media(&mut counter, (!uri.is_empty()).then_some((uri, metadata)))
        .map_err(|_| BUDGET)?;
    if counter.0 > MAX_RENDERER_EVENT_BYTES {
        Err(BUDGET)
    } else {
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RendererSubscriptionAction {
    Subscribe {
        service: RendererService,
        callbacks: Vec<String>,
        timeout_seconds: u32,
    },
    Renew {
        sid: String,
        timeout_seconds: u32,
    },
    Unsubscribe {
        sid: String,
    },
}
impl fmt::Debug for RendererSubscriptionAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RendererSubscriptionAction")
            .field("kind", &std::mem::discriminant(self))
            .finish_non_exhaustive()
    }
}
impl RendererSubscriptionAction {
    pub(crate) fn retained_bytes(&self) -> Result<usize, EngineError> {
        match self {
            Self::Subscribe { callbacks, .. } => {
                if callbacks.capacity() > 8 || callbacks.iter().any(|s| s.capacity() > 8192) {
                    return Err(BUDGET);
                }
                Ok(callbacks.capacity() * size_of::<String>()
                    + callbacks.iter().map(String::capacity).sum::<usize>())
            }
            Self::Renew { sid, .. } | Self::Unsubscribe { sid } => {
                if sid.capacity() > 128 {
                    return Err(BUDGET);
                }
                Ok(sid.capacity())
            }
        }
    }
}

/// The adapter implements this with leases from its existing shared eight-MiB ledger.
/// Retain both final event slots, shared sender scratch, and ordinary/lifecycle progress credits.
/// Declared sizes describe admission; Core does not claim that transport allocation was measured.
pub trait RendererSubscriptionLease: Send + Sync {
    fn lease_id(&self) -> u64;
    fn service(&self) -> RendererService;
    fn event_slot_bytes(&self) -> (usize, usize);
}
#[derive(Clone)]
pub struct RendererSubscriptionAdmission {
    pub lease: Arc<dyn RendererSubscriptionLease>,
    /// The actual accepted connection's deadline, bounded by Core to fifteen seconds.
    pub response_deadline_monotonic_ns: u64,
}
#[derive(Clone, PartialEq, Eq)]
pub struct RendererSubscriptionBinding {
    pub owner_id: OwnerId,
    pub renderer_epoch: u64,
    pub sid: String,
    pub generation: u64,
    pub controller_id: RendererControllerId,
    pub controller_generation: u64,
    pub callback_generation: u64,
}
impl fmt::Debug for RendererSubscriptionBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RendererSubscriptionBinding([redacted])")
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererSubscriptionInfo {
    pub binding: RendererSubscriptionBinding,
    pub timeout_seconds: u32,
    pub expires_monotonic_ns: u64,
}
#[derive(Debug, Clone)]
pub struct RendererSubscriptionResult {
    pub receipt: crate::control::Receipt,
    pub subscription: Option<RendererSubscriptionInfo>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererDeliveryOutcome {
    Delivered,
    InvalidSid,
    ConnectFailed,
    Timeout,
    MalformedResponse,
    ResponseOverflow,
    Cancelled,
    JoinFailed,
}
#[derive(Clone)]
pub struct RendererDelivery {
    pub binding: RendererSubscriptionBinding,
    pub delivery_id: u64,
    pub position: u8,
    pub sequence: u32,
    pub initial: bool,
    pub snapshot: Arc<RendererEventSnapshot>,
    pub callbacks: Arc<[String]>,
    pub grants: Arc<RendererCallbackGrantDocument>,
    pub deadline_monotonic_ns: u64,
    pub lease: Arc<dyn RendererSubscriptionLease>,
}
impl fmt::Debug for RendererDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RendererDelivery")
            .field("delivery_id", &self.delivery_id)
            .field("sequence", &self.sequence)
            .field("initial", &self.initial)
            .finish_non_exhaustive()
    }
}
#[derive(Clone)]
pub struct RendererCallbackAuthorityView {
    pub binding: RendererSubscriptionBinding,
    pub delivery_id: u64,
    pub callbacks: Arc<[String]>,
    pub grants: Arc<RendererCallbackGrantDocument>,
    pub expires_monotonic_ns: u64,
}
impl fmt::Debug for RendererCallbackAuthorityView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RendererCallbackAuthorityView([redacted])")
    }
}

#[derive(Clone)]
pub(crate) struct CallbackAuthority {
    pub controller_id: RendererControllerId,
    pub controller_generation: u64,
    pub generation: u64,
    pub expires_monotonic_ns: u64,
    pub document: Arc<RendererCallbackGrantDocument>,
}
#[derive(Clone)]
pub(crate) struct Subscription {
    pub info: RendererSubscriptionInfo,
    pub service: RendererService,
    pub authority: Arc<CallbackAuthority>,
    pub callbacks: Arc<[String]>,
    pub lease: Arc<dyn RendererSubscriptionLease>,
    pub initial_delivery_id: u64,
    pub initial: Option<Arc<RendererEventSnapshot>>,
    pub latest: Option<Arc<RendererEventSnapshot>>,
    pub observed: Arc<RendererEventSnapshot>,
    pub in_flight: Option<Arc<RendererDelivery>>,
    pub position: Option<u8>,
    pub response_deadline_ns: u64,
    pub confirmed: bool,
    pub initial_deadline_ns: Option<u64>,
    pub renewable: bool,
    pub closing: bool,
    pub next_sequence: u32,
    pub next_send_ns: u64,
}
#[derive(Clone)]
pub(crate) struct SubscriptionResultRecord {
    pub request_id: RequestId,
    pub info: Option<RendererSubscriptionInfo>,
}
#[derive(Clone, Default)]
pub(crate) struct RendererEventControl {
    pub generation: u64,
    pub serial: u64,
    pub delivery_serial: u64,
    pub callbacks: Vec<Arc<CallbackAuthority>>,
    pub subscriptions: Vec<Arc<Subscription>>,
    pub results: std::collections::VecDeque<Arc<SubscriptionResultRecord>>,
    pub admission: Option<RendererSubscriptionAdmission>,
}

fn xml_character(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= '\u{20}' && c != '\u{fffe}' && c != '\u{ffff}')
}
