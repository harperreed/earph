// ABOUTME: Validates Session-owned HTTP authority before commit and dispatch.
// ABOUTME: Shares only the current source binding with the playback worker.
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope},
    network_source::{NetworkGrantDocument, NetworkSourceBinding, NetworkSourceContext},
    session::Session,
};
use earph_network::{CheckedUrl, FetchPermit, OriginGrant, SourceBinding};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DENIED: EngineError = EngineError::new(
    ErrorCode::AuthDenied,
    "network source policy denied admission",
);
pub(crate) type LiveBinding = Arc<Mutex<Option<NetworkSourceBinding>>>;

pub(crate) fn binding(binding: &NetworkSourceBinding) -> Result<SourceBinding, EngineError> {
    SourceBinding::new(
        binding.owner_id.clone(),
        binding.session_id.clone(),
        binding.media_id.clone(),
        binding.source_generation,
        binding.grant_generation,
    )
    .map_err(|_| DENIED)
}
fn grants(
    document: &Arc<NetworkGrantDocument>,
    owner: &earph_core::ids::OwnerId,
    generation: u64,
    expiry: Instant,
) -> Result<Vec<OriginGrant>, EngineError> {
    let mut result = Vec::with_capacity(document.origins.len());
    let mut origins = Vec::with_capacity(document.origins.len());
    for (entry, grant) in document.origins.iter().enumerate() {
        let checked = CheckedUrl::new(&grant.origin).map_err(|_| DENIED)?;
        if !checked.is_origin() || origins.contains(&checked.origin()) {
            return Err(DENIED);
        }
        origins.push(checked.origin());
        result.push(
            OriginGrant::from_document(document.clone(), entry, owner.clone(), generation, expiry)
                .map_err(|_| DENIED)?,
        );
    }
    Ok(result)
}
pub(crate) fn validate(
    session: &Session,
    command: &CommandEnvelope,
    clock: &Instant,
) -> Result<(), EngineError> {
    match &command.payload {
        Command::NetworkGrantsReplace { .. } => {
            let (generation, expires, document) = session.network_grants().ok_or(DENIED)?;
            let owner = session.snapshot().owner_id.ok_or(DENIED)?;
            let expiry = clock
                .checked_add(Duration::from_nanos(expires))
                .ok_or(DENIED)?;
            grants(&document, &owner, generation, expiry)?;
            Ok(())
        }
        Command::NetworkStartHttpFlac { .. } => {
            let context = session.network_source().ok_or(DENIED)?;
            permit(session, &context, clock).map(|_| ())
        }
        Command::NetworkGrantsRevoke { .. } => Ok(()),
        _ => Ok(()),
    }
}
pub(crate) fn permit(
    session: &Session,
    context: &NetworkSourceContext,
    clock: &Instant,
) -> Result<FetchPermit, EngineError> {
    let now_ns = clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    if !session.network_authority_valid(&context.binding, now_ns) {
        return Err(DENIED);
    }
    let live = session.network_source().ok_or(DENIED)?;
    if !Arc::ptr_eq(&live.descriptor, &context.descriptor) || live.binding != context.binding {
        return Err(DENIED);
    }
    let requested = binding(&context.binding)?;
    let current = binding(&live.binding)?;
    let expiry = clock
        .checked_add(Duration::from_nanos(context.expires_monotonic_ns))
        .ok_or(DENIED)?;
    FetchPermit::new(
        CheckedUrl::new(context.descriptor.uri()).map_err(|_| DENIED)?,
        requested,
        &current,
        grants(
            &context.grants,
            &context.binding.owner_id,
            context.binding.grant_generation,
            expiry,
        )?,
        Instant::now(),
    )
    .map_err(|_| DENIED)
}
