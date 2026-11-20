// ABOUTME: Admits exact origins against immutable owner source grants.
// ABOUTME: Rejects unsafe destinations before constructing any transport.
use earph_core::{
    ids::{OwnerId, SessionId, StagedMediaId},
    network_source::NetworkGrantDocument,
};
use std::{net::IpAddr, sync::Arc, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    InvalidUrl,
    InvalidBinding,
    InvalidGrant,
    NotGranted,
    Expired,
    Budget,
}
impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "network admission denied: {self:?}")
    }
}
impl std::error::Error for PolicyError {}

#[derive(Clone, PartialEq, Eq)]
pub struct Origin {
    pub(crate) scheme: String,
    pub(crate) host: String,
    pub(crate) port: u16,
}
impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Origin([redacted])")
    }
}
#[derive(Clone)]
pub struct CheckedUrl(pub(crate) reqwest::Url);
impl std::fmt::Debug for CheckedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CheckedUrl([redacted])")
    }
}
impl CheckedUrl {
    fn validate_text(text: &str) -> Result<(), PolicyError> {
        if text.len() > 8192 {
            return Err(PolicyError::Budget);
        }
        if text.chars().any(|c| c.is_control() || c.is_whitespace()) || text.contains(['#', '\\']) {
            return Err(PolicyError::InvalidUrl);
        }
        Ok(())
    }
    pub fn new(text: &str) -> Result<Self, PolicyError> {
        Self::validate_text(text)?;
        let (scheme, remainder) = text.split_once("://").ok_or(PolicyError::InvalidUrl)?;
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(PolicyError::InvalidUrl);
        }
        let authority = remainder
            .split(['/', '?'])
            .next()
            .ok_or(PolicyError::InvalidUrl)?;
        if authority.is_empty() || authority.contains('@') {
            return Err(PolicyError::InvalidUrl);
        }
        let url = reqwest::Url::parse(text).map_err(|_| PolicyError::InvalidUrl)?;
        if url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(PolicyError::InvalidUrl);
        }
        Ok(Self(url))
    }
    pub(crate) fn redirected(&self, reference: &str) -> Result<Self, PolicyError> {
        Self::validate_text(reference)?;
        // Validate raw authorities before URL parsing can erase empty userinfo.
        if reference.starts_with("//") {
            return Self::new(&format!("{}:{reference}", self.0.scheme()));
        }
        if reference
            .split(['/', '?'])
            .next()
            .unwrap_or_default()
            .contains(':')
        {
            return Self::new(reference);
        }
        let next = self
            .0
            .join(reference)
            .map_err(|_| PolicyError::InvalidUrl)?;
        Self::new(next.as_str())
    }
    pub fn origin(&self) -> Origin {
        let host = self
            .0
            .host_str()
            .expect("checked host")
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        Origin {
            scheme: self.0.scheme().into(),
            host,
            port: self.0.port_or_known_default().expect("HTTP port"),
        }
    }
    /// Whether this URL identifies only an origin, without a resource or query.
    pub fn is_origin(&self) -> bool {
        self.0.path() == "/" && self.0.query().is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBinding {
    owner: OwnerId,
    session: SessionId,
    media: StagedMediaId,
    source_generation: u64,
    grant_generation: u64,
}
impl SourceBinding {
    pub fn new(
        owner: OwnerId,
        session: SessionId,
        media: StagedMediaId,
        source_generation: u64,
        grant_generation: u64,
    ) -> Result<Self, PolicyError> {
        if source_generation == 0 || grant_generation == 0 {
            return Err(PolicyError::InvalidBinding);
        }
        Ok(Self {
            owner,
            session,
            media,
            source_generation,
            grant_generation,
        })
    }
    pub fn owner(&self) -> &OwnerId {
        &self.owner
    }
    pub fn session(&self) -> &SessionId {
        &self.session
    }
    pub fn media(&self) -> &StagedMediaId {
        &self.media
    }
    pub fn source_generation(&self) -> u64 {
        self.source_generation
    }
    pub fn grant_generation(&self) -> u64 {
        self.grant_generation
    }
}

#[derive(Clone)]
pub struct OriginGrant {
    pub(crate) origin: Origin,
    owner: OwnerId,
    generation: u64,
    expiry: Instant,
    backing: GrantBacking,
}
struct GrantValues {
    pins: Vec<IpAddr>,
    roots: Vec<Vec<u8>>,
}
#[derive(Clone)]
enum GrantBacking {
    Owned(Arc<GrantValues>),
    Shared {
        document: Arc<NetworkGrantDocument>,
        entry: usize,
    },
}
impl GrantBacking {
    fn values(&self) -> (&[IpAddr], &[Vec<u8>]) {
        match self {
            Self::Owned(values) => (&values.pins, &values.roots),
            Self::Shared { document, entry } => {
                let value = &document.origins[*entry];
                (&value.pins, &value.roots_der)
            }
        }
    }
}
impl std::fmt::Debug for OriginGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OriginGrant([redacted])")
    }
}
impl OriginGrant {
    /// Roots contain DER certificates. HTTPS uses only these explicit roots.
    pub fn new(
        origin: Origin,
        owner: OwnerId,
        generation: u64,
        pins: Vec<IpAddr>,
        expiry: Instant,
        roots: Vec<Vec<u8>>,
    ) -> Result<Self, PolicyError> {
        Self::from_backing(
            origin,
            owner,
            generation,
            expiry,
            GrantBacking::Owned(Arc::new(GrantValues { pins, roots })),
        )
    }
    /// Borrows one exact immutable entry without copying pins or certificate bytes.
    pub fn from_document(
        document: Arc<NetworkGrantDocument>,
        entry: usize,
        owner: OwnerId,
        generation: u64,
        expiry: Instant,
    ) -> Result<Self, PolicyError> {
        document.retained_bytes().map_err(|_| PolicyError::Budget)?;
        let grant = document
            .origins
            .get(entry)
            .ok_or(PolicyError::InvalidGrant)?;
        let url = CheckedUrl::new(&grant.origin)?;
        if !url.is_origin() {
            return Err(PolicyError::InvalidGrant);
        }
        Self::from_backing(
            url.origin(),
            owner,
            generation,
            expiry,
            GrantBacking::Shared { document, entry },
        )
    }
    pub fn pins(&self) -> &[IpAddr] {
        self.backing.values().0
    }
    pub fn roots_der(&self) -> &[Vec<u8>] {
        self.backing.values().1
    }
    fn from_backing(
        origin: Origin,
        owner: OwnerId,
        generation: u64,
        expiry: Instant,
        backing: GrantBacking,
    ) -> Result<Self, PolicyError> {
        let (pins, roots) = backing.values();
        if pins.len() > 8
            || roots.len() > 64
            || roots
                .iter()
                .try_fold(0usize, |n, root| n.checked_add(root.len()))
                .is_none_or(|n| n > 1024 * 1024)
        {
            return Err(PolicyError::Budget);
        }
        if generation == 0 || pins.is_empty() || pins.iter().any(|ip| !permitted_address(*ip)) {
            return Err(PolicyError::InvalidGrant);
        }
        if let Ok(literal) = origin.host.parse::<IpAddr>()
            && pins.iter().any(|ip| *ip != literal)
        {
            return Err(PolicyError::InvalidGrant);
        }
        if origin.scheme == "https" && roots.is_empty() {
            return Err(PolicyError::InvalidGrant);
        }
        if !roots.is_empty() {
            let certificates = roots
                .iter()
                .map(|root| reqwest::Certificate::from_der(root))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| PolicyError::InvalidGrant)?;
            // Client construction parses the root store. Certificate::from_der only retains bytes.
            reqwest::Client::builder()
                .no_proxy()
                .tls_certs_only(certificates)
                .build()
                .map_err(|_| PolicyError::InvalidGrant)?;
        }
        Ok(Self {
            origin,
            owner,
            generation,
            expiry,
            backing,
        })
    }
}

#[derive(Debug, Clone)]
pub struct FetchPermit {
    pub(crate) url: CheckedUrl,
    pub(crate) binding: SourceBinding,
    grants: Vec<OriginGrant>,
}
impl FetchPermit {
    pub fn binding(&self) -> &SourceBinding {
        &self.binding
    }
    pub fn new(
        url: CheckedUrl,
        binding: SourceBinding,
        current: &SourceBinding,
        grants: Vec<OriginGrant>,
        now: Instant,
    ) -> Result<Self, PolicyError> {
        if &binding != current {
            return Err(PolicyError::InvalidBinding);
        }
        if grants.len() > 8 {
            return Err(PolicyError::Budget);
        }
        for (index, grant) in grants.iter().enumerate() {
            if grant.owner != binding.owner
                || grant.generation != binding.grant_generation
                || grants[..index]
                    .iter()
                    .any(|other| other.origin == grant.origin)
            {
                return Err(PolicyError::InvalidGrant);
            }
        }
        let permit = Self {
            url,
            binding,
            grants,
        };
        permit.grant_for(&permit.url, now)?;
        Ok(permit)
    }
    pub(crate) fn grant_for(
        &self,
        url: &CheckedUrl,
        now: Instant,
    ) -> Result<&OriginGrant, PolicyError> {
        let grant = self
            .grants
            .iter()
            .find(|grant| grant.origin == url.origin())
            .ok_or(PolicyError::NotGranted)?;
        if grant.expiry <= now {
            return Err(PolicyError::Expired);
        }
        Ok(grant)
    }
}

fn permitted_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 192 && ((b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && ((b == 18 || b == 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return permitted_address(IpAddr::V4(mapped));
            }
            let s = ip.segments();
            // Admit ULA and ordinary global unicast, excluding transition and documentation ranges.
            (s[0] & 0xfe00 == 0xfc00)
                || (s[0] & 0xe000 == 0x2000
                    && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                    && s[0] != 0x2002
                    && !(s[0] == 0x3fff && s[1] < 0x1000))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_redirect_paths_and_queries_preserve_at_signs() {
        let base = CheckedUrl::new("http://media.test/a/b?old=1").unwrap();
        for (reference, expected) in [
            (
                "../track@artist?token=@value",
                "http://media.test/track@artist?token=@value",
            ),
            ("?token=@value", "http://media.test/a/b?token=@value"),
            (
                "/http:clip@name?x=http://@data",
                "http://media.test/http:clip@name?x=http://@data",
            ),
            ("//other.test/a@b", "http://other.test/a@b"),
        ] {
            assert_eq!(base.redirected(reference).unwrap().0.as_str(), expected);
        }
    }
}
