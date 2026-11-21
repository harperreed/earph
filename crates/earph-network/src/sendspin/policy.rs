// ABOUTME: Checks exact expiring connection grants without playback bindings.
// ABOUTME: Reuses the existing destination policy and never resolves hostnames.
use super::DriverError;
use earph_core::ids::OwnerId;
use std::{net::IpAddr, time::Instant};
#[derive(Clone, PartialEq, Eq)]
pub struct SendspinConnectPermit {
    pub(crate) owner: OwnerId,
    pub(crate) connection: u64,
    pub(crate) grant: u64,
    pub(crate) expires: Instant,
    pub(crate) endpoint: String,
    pub(crate) pins: Vec<IpAddr>,
    pub(crate) port: u16,
}
impl std::fmt::Debug for SendspinConnectPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SendspinConnectPermit([REDACTED])")
    }
}
impl SendspinConnectPermit {
    pub fn new(
        owner: OwnerId,
        connection: u64,
        grant: u64,
        expires: Instant,
        endpoint: &str,
        pins: Vec<IpAddr>,
    ) -> Result<Self, DriverError> {
        if endpoint.len() > 8192
            || endpoint
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || endpoint.contains('\\')
            || endpoint.contains('%')
        {
            return Err(DriverError::Admission);
        }
        let url = reqwest::Url::parse(endpoint).map_err(|_| DriverError::Admission)?;
        if url.scheme() == "wss" {
            return Err(DriverError::TlsUnsupported);
        }
        if url.scheme() != "ws"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with('/')
        {
            return Err(DriverError::Admission);
        }
        let authority = endpoint
            .strip_prefix("ws://")
            .ok_or(DriverError::Admission)?
            .split('/')
            .next()
            .ok_or(DriverError::Admission)?;
        let port = authority
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .ok_or(DriverError::Admission)?;
        if url.host_str().is_none()
            || authority.contains('@')
            || connection == 0
            || grant == 0
            || pins.is_empty()
            || pins.len() > 8
            || pins.iter().any(|p| !crate::policy::permitted_address(*p))
        {
            return Err(DriverError::Admission);
        }
        if let Some(host) = url.host_str() {
            let literal = host.trim_matches(['[', ']']).parse::<IpAddr>();
            if let Ok(ip) = literal
                && pins.iter().any(|p| *p != ip)
            {
                return Err(DriverError::Admission);
            }
        }
        if expires <= Instant::now() {
            return Err(DriverError::Expired);
        }
        Ok(Self {
            owner,
            connection,
            grant,
            expires,
            endpoint: endpoint.to_owned(),
            pins: pins.into_boxed_slice().into_vec(),
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admitted_pin_storage_discards_unbounded_caller_capacity() {
        let mut pins = Vec::with_capacity(9);
        pins.push("10.1.2.3".parse().unwrap());
        let permit = SendspinConnectPermit::new(
            OwnerId::new("fixture").unwrap(),
            1,
            1,
            Instant::now() + std::time::Duration::from_secs(1),
            "ws://fixture.invalid:42/",
            pins,
        )
        .unwrap();
        assert!(
            permit.pins.capacity() <= 8,
            "retained pins must fit their admitted capacity"
        );
    }
}
