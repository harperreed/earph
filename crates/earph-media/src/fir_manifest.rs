// ABOUTME: Records explicit FIR correction provenance without changing authored WAV bytes.
// ABOUTME: Builds one exact preamp and convolution graph for the declared processing context.
use crate::{
    MAX_FIR_WAV_BYTES,
    correction_text::{ImportContext, ImportMetadata, validate_metadata},
};
use earph_core::{
    EngineError, ErrorCode,
    audio::PcmRepresentation,
    dsp_nodes::FirAssetReference,
    graph::{Edge, FiniteGraphScalar, GraphDocument, Node, NodeKind, PortFormat},
    ids::NodeId,
    profile_control::CorrectionNormalization,
    safety::SampleDomain,
};
use serde::{Deserialize, Serialize};

pub const MAX_FIR_MANIFEST_BYTES: usize = 8 * 1024;
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid FIR correction manifest");
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "FIR correction manifest exceeds its bounds",
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirCorrectionManifest {
    pub schema_version: u32,
    pub asset: FirAssetReference,
    pub source_bytes: usize,
    pub context: ImportContext,
    pub metadata: ImportMetadata,
    pub preamp_db: FiniteGraphScalar,
    pub preamp_explicit: bool,
    pub normalization: CorrectionNormalization,
    pub channel_map: Vec<usize>,
}
impl FirCorrectionManifest {
    pub fn new(
        asset: FirAssetReference,
        source_bytes: usize,
        context: ImportContext,
        metadata: ImportMetadata,
        preamp_db: FiniteGraphScalar,
    ) -> Result<Self, EngineError> {
        let channel_map = (0..asset.channels.count()).collect();
        let manifest = Self {
            schema_version: 1,
            asset,
            source_bytes,
            context,
            metadata,
            preamp_db,
            preamp_explicit: true,
            normalization: CorrectionNormalization::Preserved,
            channel_map,
        };
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported FIR manifest version",
            ));
        }
        self.asset.validate()?;
        self.context
            .validate()
            .map_err(|e| EngineError::new(ErrorCode::AssetInvalid, e.message))?;
        validate_metadata(&self.metadata)
            .map_err(|e| EngineError::new(ErrorCode::AssetInvalid, e.message))?;
        let capacity = [
            &self.metadata.headphone_model,
            &self.metadata.measurement_provider,
            &self.metadata.target,
            &self.metadata.parameter_set,
            &self.metadata.revision,
            &self.metadata.redistribution_license,
        ]
        .into_iter()
        .flatten()
        .try_fold(
            size_of::<Self>() + self.channel_map.capacity() * size_of::<usize>(),
            |n, s| n.checked_add(s.capacity()),
        )
        .ok_or(BUDGET)?;
        if capacity > MAX_FIR_MANIFEST_BYTES
            || self.channel_map.capacity() > 4
            || self.source_bytes > MAX_FIR_WAV_BYTES
        {
            return Err(BUDGET);
        }
        if self.source_bytes == 0
            || self.asset.sample_rate_hz != self.context.sample_rate_hz
            || self.asset.channels != self.context.channels
            || !self.preamp_explicit
            || self.preamp_db.get().abs() > 150.0
            || !self
                .channel_map
                .iter()
                .copied()
                .eq(0..self.asset.channels.count())
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub fn graph(&self) -> Result<GraphDocument, EngineError> {
        self.validate()?;
        let format = PortFormat {
            domain: SampleDomain::Pcm,
            sample_rate_hz: self.context.sample_rate_hz,
            channels: self.context.channels,
            precision: PcmRepresentation::Float64,
            max_quantum_frames: self.context.quantum_frames,
        };
        let ids = [
            NodeId::new("input")?,
            NodeId::new("preamp")?,
            NodeId::new("fir")?,
            NodeId::new("output")?,
        ];
        let kinds = [
            NodeKind::Input,
            NodeKind::Gain {
                gain_linear: FiniteGraphScalar::new(10_f64.powf(self.preamp_db.get() / 20.0))?,
            },
            NodeKind::Convolution {
                asset: self.asset.clone(),
            },
            NodeKind::Output,
        ];
        let nodes = ids
            .iter()
            .cloned()
            .zip(kinds)
            .map(|(id, kind)| Node { id, kind, format })
            .collect();
        let edges = ids
            .windows(2)
            .map(|p| Edge {
                from: p[0].clone(),
                to: p[1].clone(),
                from_port: 0,
                to_port: 0,
            })
            .collect();
        let graph = GraphDocument {
            schema_version: 1,
            output: ids[3].clone(),
            nodes,
            edges,
        };
        graph.validate_structure()?;
        Ok(graph)
    }
}
pub fn read_fir_manifest(bytes: &[u8]) -> Result<FirCorrectionManifest, EngineError> {
    if bytes.len() > MAX_FIR_MANIFEST_BYTES {
        return Err(BUDGET);
    }
    let manifest: FirCorrectionManifest = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    manifest.validate()?;
    Ok(manifest)
}
