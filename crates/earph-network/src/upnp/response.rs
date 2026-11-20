// ABOUTME: Encodes exact service outputs and static UPnP faults without private error text.
// ABOUTME: Validates wire scalar types without performing any owner action or volume conversion.
use super::{Service, SoapError, SoapRequest, contract};
use std::fmt::Write;

pub const MAX_RESPONSE_BYTES: usize = 524288;
const PREFIX: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpnpFault {
    InvalidAction,
    InvalidArguments,
    ActionFailed,
    InvalidInstance(Service),
    PlaySpeedNotSupported,
    SeekModeNotSupported,
    IllegalSeekTarget,
    InvalidConnection,
}
impl UpnpFault {
    pub const fn code(self) -> u16 {
        match self {
            Self::InvalidAction => 401,
            Self::InvalidArguments => 402,
            Self::ActionFailed => 501,
            Self::InvalidInstance(Service::AvTransport) => 718,
            Self::InvalidInstance(_) => 702,
            Self::PlaySpeedNotSupported => 717,
            Self::SeekModeNotSupported => 710,
            Self::IllegalSeekTarget => 711,
            Self::InvalidConnection => 706,
        }
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::InvalidAction => "Invalid Action",
            Self::InvalidArguments => "Invalid Args",
            Self::ActionFailed => "Action Failed",
            Self::InvalidInstance(_) => "Invalid InstanceID",
            Self::PlaySpeedNotSupported => "Play speed not supported",
            Self::SeekModeNotSupported => "Seek mode not supported",
            Self::IllegalSeekTarget => "Illegal seek target",
            Self::InvalidConnection => "Invalid connection reference",
        }
    }
}
impl From<SoapError> for UpnpFault {
    fn from(error: SoapError) -> Self {
        match error {
            SoapError::InvalidAction => Self::InvalidAction,
            _ => Self::InvalidArguments,
        }
    }
}

fn scalar(variable: &str, value: &str) -> bool {
    if !value.chars().all(super::soap::valid_xml_character) {
        return false;
    }
    match contract::variable_type(variable) {
        "ui4" => value.parse::<u32>().is_ok(),
        "i4" => value.parse::<i32>().is_ok(),
        "ui2" => value.parse::<u16>().is_ok_and(|v| v <= 100),
        "i2" => value
            .parse::<i16>()
            .is_ok_and(|v| (-32767..=0).contains(&v)),
        "boolean" => matches!(value, "0" | "1" | "true" | "false" | "yes" | "no"),
        _ => true,
    }
}
impl SoapRequest {
    /// Validate typed input before passing the request to an owner bridge.
    pub fn validate_arguments(&self) -> Result<(), UpnpFault> {
        let spec = self
            .service
            .actions()
            .iter()
            .find(|a| a.name == self.name)
            .ok_or(UpnpFault::InvalidAction)?;
        for (name, value) in &self.arguments {
            if !scalar(contract::input_variable(spec, name), value) {
                return Err(UpnpFault::InvalidArguments);
            }
        }
        if self
            .argument("InstanceID")
            .is_some_and(|v| v.parse::<u32>() != Ok(0))
        {
            return Err(UpnpFault::InvalidInstance(self.service));
        }
        if self.argument("Channel").is_some_and(|v| v != "Master") {
            return Err(UpnpFault::InvalidArguments);
        }
        if self.name == "Play" && self.argument("Speed") != Some("1") {
            return Err(UpnpFault::PlaySpeedNotSupported);
        }
        Ok(())
    }

    /// Apply the renderer's declared nonseekable, single-source contract after state validation.
    pub fn nonseekable_fault(&self) -> Option<UpnpFault> {
        if self.service != Service::AvTransport {
            return None;
        }
        match self.name {
            "Seek" => Some(UpnpFault::SeekModeNotSupported),
            "Next" | "Previous" => Some(UpnpFault::IllegalSeekTarget),
            _ => None,
        }
    }
}

pub fn encode_fault(fault: UpnpFault) -> Vec<u8> {
    format!("{PREFIX}<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{}</errorCode><errorDescription>{}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>", fault.code(), fault.description()).into_bytes()
}

pub fn encode_response(
    request: &SoapRequest,
    values: &[(&str, &str)],
) -> Result<Vec<u8>, SoapError> {
    let spec = request
        .service
        .actions()
        .iter()
        .find(|a| a.name == request.name)
        .ok_or(SoapError::InvalidAction)?;
    if values.len() != spec.output.len() {
        return Err(SoapError::InvalidArguments);
    }
    let opening = format!(
        "<u:{}Response xmlns:u=\"{}\">",
        request.name,
        request.service.namespace()
    );
    let closing = format!("</u:{}Response></s:Body></s:Envelope>", request.name);
    let mut size = PREFIX.len() + opening.len() + closing.len();
    for ((name, value), (expected, variable)) in values.iter().zip(spec.output) {
        if value.len() > MAX_RESPONSE_BYTES {
            return Err(SoapError::ResourceBudgetExceeded);
        }
        if name != expected || !scalar(variable, value) {
            return Err(SoapError::InvalidArguments);
        }
        let escaped: usize = value
            .bytes()
            .map(|byte| match byte {
                b'<' | b'>' => 4,
                b'&' => 5,
                b'\'' | b'"' => 6,
                _ => 1,
            })
            .sum();
        size = size
            .checked_add(escaped + name.len() * 2 + 5)
            .ok_or(SoapError::ResourceBudgetExceeded)?;
        if size > MAX_RESPONSE_BYTES {
            return Err(SoapError::ResourceBudgetExceeded);
        }
    }
    let mut xml = String::with_capacity(size);
    xml.push_str(PREFIX);
    xml.push_str(&opening);
    for (name, value) in values {
        write!(
            xml,
            "<{name}>{}</{name}>",
            quick_xml::escape::escape(*value)
        )
        .unwrap();
    }
    xml.push_str(&closing);
    debug_assert_eq!(xml.len(), size);
    Ok(xml.into_bytes())
}
