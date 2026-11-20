// ABOUTME: Checks output volume and fidelity before a route can unmute.
// ABOUTME: Keeps fixed-output arming bound to the current device and profile.
use crate::{
    EngineError, ErrorCode,
    ids::{DeviceId, ProfileId},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FidelityPolicy {
    StrictPreservation,
    CompatibleConversion,
    ExpertOverride,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeMode {
    Software,
    Hardware,
    Fixed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleDomain {
    Pcm,
    Dsd,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputContext {
    pub device_id: DeviceId,
    pub profile_id: ProfileId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CapsDocument")]
pub struct VolumePolicy {
    software_cap_db: f64,
    hardware_cap_db: Option<f64>,
    #[serde(skip)]
    armed_for: Option<OutputContext>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapsDocument {
    software_cap_db: f64,
    hardware_cap_db: Option<f64>,
}
impl TryFrom<CapsDocument> for VolumePolicy {
    type Error = EngineError;
    fn try_from(document: CapsDocument) -> Result<Self, EngineError> {
        Self::new(document.software_cap_db, document.hardware_cap_db)
    }
}
impl Default for VolumePolicy {
    fn default() -> Self {
        Self {
            software_cap_db: 0.0,
            hardware_cap_db: None,
            armed_for: None,
        }
    }
}

impl VolumePolicy {
    /// Validate exact zero without changing finite-only gain admission.
    pub fn validate_software_zero(
        &self,
        mode: VolumeMode,
        domain: SampleDomain,
        fidelity: FidelityPolicy,
    ) -> Result<(), EngineError> {
        if mode != VolumeMode::Software
            || domain != SampleDomain::Pcm
            || fidelity == FidelityPolicy::StrictPreservation
        {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "exact zero requires compatible software PCM attenuation",
            ));
        }
        Ok(())
    }
    pub fn new(software_cap_db: f64, hardware_cap_db: Option<f64>) -> Result<Self, EngineError> {
        if !software_cap_db.is_finite()
            || !(-120.0..=0.0).contains(&software_cap_db)
            || hardware_cap_db.is_some_and(|v| !v.is_finite() || !(-200.0..=24.0).contains(&v))
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "invalid persistent volume cap",
            ));
        }
        Ok(Self {
            software_cap_db,
            hardware_cap_db,
            armed_for: None,
        })
    }
    pub fn software_cap_db(&self) -> f64 {
        self.software_cap_db
    }
    pub fn hardware_cap_db(&self) -> Option<f64> {
        self.hardware_cap_db
    }
    pub fn arm_fixed(&mut self, context: OutputContext) {
        self.armed_for = Some(context);
    }
    pub fn disarm(&mut self) {
        self.armed_for = None;
    }
    pub fn validate_unmute(
        &self,
        context: &OutputContext,
        mode: VolumeMode,
        domain: SampleDomain,
        fidelity: FidelityPolicy,
        software_gain_db: f64,
        hardware_readback_db: Option<f64>,
    ) -> Result<(), EngineError> {
        self.validate_gain_constraints(
            mode,
            domain,
            fidelity,
            software_gain_db,
            hardware_readback_db,
        )?;
        if mode == VolumeMode::Fixed && self.armed_for.as_ref() != Some(context) {
            return Err(EngineError::new(
                ErrorCode::SafetyPolicyBlocked,
                "output volume has not satisfied its safety policy",
            ));
        }
        Ok(())
    }
    /// Validate local PCM preparation without copying current arming into prospective intent.
    pub fn validate_profile_preparation(
        &self,
        current_context: &OutputContext,
        mode: VolumeMode,
        fidelity: FidelityPolicy,
        current_gain_db: f64,
        prospective: &VolumePolicy,
        prospective_gain_db: f64,
    ) -> Result<(), EngineError> {
        if mode == VolumeMode::Hardware {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "hardware profile preparation requires actual set/readback support",
            ));
        }
        self.validate_unmute(
            current_context,
            mode,
            SampleDomain::Pcm,
            fidelity,
            current_gain_db,
            None,
        )?;
        prospective.validate_gain_constraints(
            mode,
            SampleDomain::Pcm,
            fidelity,
            prospective_gain_db,
            None,
        )
    }
    pub(crate) fn validate_gain_constraints(
        &self,
        mode: VolumeMode,
        domain: SampleDomain,
        fidelity: FidelityPolicy,
        software_gain_db: f64,
        hardware_readback_db: Option<f64>,
    ) -> Result<(), EngineError> {
        let blocked = EngineError::new(
            ErrorCode::SafetyPolicyBlocked,
            "output volume has not satisfied its safety policy",
        );
        let conflict = EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "volume mode conflicts with sample preservation",
        );
        if !software_gain_db.is_finite() || !(-120.0..=0.0).contains(&software_gain_db) {
            return Err(blocked);
        }
        if software_gain_db > self.software_cap_db
            || hardware_readback_db.is_some_and(|level| !level.is_finite())
        {
            return Err(blocked);
        }
        if self.hardware_cap_db.is_some_and(|cap| {
            hardware_readback_db.is_none_or(|level| !level.is_finite() || level > cap)
        }) {
            return Err(blocked);
        }
        match mode {
            VolumeMode::Software => {
                if domain == SampleDomain::Dsd
                    || (fidelity == FidelityPolicy::StrictPreservation && software_gain_db != 0.0)
                {
                    return Err(conflict);
                }
            }
            VolumeMode::Hardware => {
                if software_gain_db != 0.0 {
                    return Err(conflict);
                }
                if self.hardware_cap_db.is_none()
                    || hardware_readback_db.is_none_or(|level| !level.is_finite())
                {
                    return Err(blocked);
                }
            }
            VolumeMode::Fixed => {
                if software_gain_db != 0.0 {
                    return Err(conflict);
                }
            }
        }
        Ok(())
    }
}
