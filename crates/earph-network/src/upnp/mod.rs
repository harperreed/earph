// ABOUTME: Defines the bounded UPnP wire boundary without granting playback authority.
// ABOUTME: Keeps SOAP payloads out of generic debug output and delegates execution to Core.
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    AvTransport,
    RenderingControl,
    ConnectionManager,
}

impl Service {
    pub const ALL: [Self; 3] = [
        Self::AvTransport,
        Self::RenderingControl,
        Self::ConnectionManager,
    ];
    pub const fn slug(self) -> &'static str {
        match self {
            Self::AvTransport => "avt",
            Self::RenderingControl => "rcs",
            Self::ConnectionManager => "cm",
        }
    }

    pub const fn namespace(self) -> &'static str {
        match self {
            Self::AvTransport => "urn:schemas-upnp-org:service:AVTransport:1",
            Self::RenderingControl => "urn:schemas-upnp-org:service:RenderingControl:1",
            Self::ConnectionManager => "urn:schemas-upnp-org:service:ConnectionManager:1",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SoapError {
    InvalidAction,
    InvalidArguments,
    MalformedXml,
    ResourceBudgetExceeded,
}

pub struct SoapRequest {
    pub service: Service,
    name: &'static str,
    arguments: Vec<(String, String)>,
}

impl SoapRequest {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn argument(&self, name: &str) -> Option<&str> {
        self.arguments
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

impl fmt::Debug for SoapRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoapRequest")
            .field("service", &self.service)
            .field("action", &self.name)
            .finish_non_exhaustive()
    }
}

mod soap;
pub use soap::decode_soap;
mod contract;
mod description;
pub use description::{device_description, service_description};
mod response;
pub use response::{MAX_RESPONSE_BYTES, UpnpFault, encode_fault, encode_response};
mod http;
pub use http::{ADAPTER_MEMORY_BYTES, DeviceIdentity, HttpSurface, PendingAction, SurfaceStats};
mod ssdp;
pub use ssdp::{
    Advertisement, SSDP_GROUP, Search, SearchError, SsdpEndpoint, SsdpScheduler, parse_search,
};
