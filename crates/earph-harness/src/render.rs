// ABOUTME: Runs a prepared, bounded integer renderer against virtual output.
// ABOUTME: Injects scheduled faults and preserves partial captures on failure.
use crate::{
    fixture::{FixtureSource, FixtureSpec},
    sha256,
    virtual_output::VirtualOutput,
};
use earph_core::{EngineError, ErrorCode, audio::*, ids::Epochs};
use serde::{Deserialize, Serialize};

const BUDGET: EngineError =
    EngineError::new(ErrorCode::ResourceBudgetExceeded, "render budget exceeded");
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "invalid render request or fault schedule",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FaultKind {
    ShortWrite { max_frames: usize },
    ZeroProgress,
    Underrun,
    BackendFailure,
    MalformedChunk,
    StaleEpoch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduledFault {
    pub at_frame: u64,
    pub fault: FaultKind,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultSchedule {
    pub schema_version: u32,
    pub faults: Vec<ScheduledFault>,
}
impl Default for FaultSchedule {
    fn default() -> Self {
        Self {
            schema_version: 1,
            faults: vec![],
        }
    }
}
impl FaultSchedule {
    fn validate(&self, frames: u64) -> Result<(), EngineError> {
        if self.schema_version != 1 || self.faults.len() > 128 {
            return Err(INVALID);
        }
        let mut prior = None;
        for fault in &self.faults {
            if fault.at_frame >= frames
                || prior.is_some_and(|frame| frame >= fault.at_frame)
                || matches!(fault.fault, FaultKind::ShortWrite { max_frames: 0 })
            {
                return Err(INVALID);
            }
            prior = Some(fault.at_frame);
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub fixture: FixtureSpec,
    pub block_frames: usize,
    pub budget: MemoryBudget,
    pub epochs: Epochs,
    pub faults: FaultSchedule,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderEventKind {
    Prepared,
    Written { frames: usize },
    Fault { fault: FaultKind },
    Stopped { code: ErrorCode },
    Finished,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderEvent {
    pub at_frame: u64,
    pub epochs: Epochs,
    pub event: RenderEventKind,
}
#[derive(Debug, Serialize)]
pub struct RenderReport {
    pub status: &'static str,
    pub error: Option<EngineError>,
    pub requested_format: PcmFormat,
    pub resolved_format: Option<PcmFormat>,
    pub observed_format: Option<PcmFormat>,
    pub measurement_boundary: &'static str,
    pub epochs: Epochs,
    pub output_frames: u64,
    pub decoded_frames: u64,
    pub elapsed_ns: u128,
    pub payload_sha256: String,
    pub events: Vec<RenderEvent>,
    #[serde(skip_serializing)]
    pub capture: Vec<i32>,
}
impl RenderReport {
    pub fn capture_bytes(&self) -> Vec<u8> {
        let width = self.requested_format.representation().bytes();
        let mut bytes = Vec::with_capacity(self.capture.len() * width);
        for value in &self.capture {
            bytes.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        bytes
    }
    fn unprepared(format: PcmFormat, epochs: Epochs, error: EngineError) -> Self {
        Self {
            status: "fail",
            error: Some(error),
            requested_format: format,
            resolved_format: None,
            observed_format: None,
            measurement_boundary: "engine_output",
            epochs,
            output_frames: 0,
            decoded_frames: 0,
            elapsed_ns: 0,
            payload_sha256: sha256(&[]),
            events: vec![],
            capture: vec![],
        }
    }
}
#[derive(Debug)]
pub struct RenderFailure {
    pub error: EngineError,
    pub report: Box<RenderReport>,
}

pub struct PreparedRender {
    request: RenderRequest,
    source: FixtureSource,
    input: Option<PcmBlock>,
    block: PcmBlock,
    output: VirtualOutput,
    events: Vec<RenderEvent>,
    decoded_frames: u64,
    offset: usize,
    fault_index: usize,
    write_limit: usize,
    processed: bool,
}
impl PreparedRender {
    pub fn prepare(request: RenderRequest) -> Result<Self, EngineError> {
        Self::prepare_input(request, None)
    }
    fn prepare_input(request: RenderRequest, input: Option<PcmBlock>) -> Result<Self, EngineError> {
        if request.block_frames == 0 {
            return Err(INVALID);
        }
        request.faults.validate(request.fixture.frames)?;
        let frames = usize::try_from(request.fixture.frames).map_err(|_| BUDGET)?;
        let events = frames
            .checked_add(request.faults.faults.len() * 2 + 4)
            .ok_or(BUDGET)?;
        let samples = request
            .fixture
            .format
            .scalar_count(request.fixture.frames)?;
        let capture_bytes = samples.checked_mul(4).ok_or(BUDGET)?;
        let block_bytes = request
            .fixture
            .format
            .storage_bytes(request.block_frames as u64)?;
        let input_bytes = if input.is_some() {
            request
                .fixture
                .format
                .storage_bytes(request.fixture.frames)?
        } else {
            0
        };
        let bytes = events
            .checked_mul(std::mem::size_of::<RenderEvent>())
            .and_then(|n| n.checked_add(capture_bytes))
            .and_then(|n| n.checked_add(block_bytes))
            .and_then(|n| n.checked_add(input_bytes))
            .ok_or(BUDGET)?;
        request.budget.admit(bytes)?;
        let source = FixtureSource::new(request.fixture.clone())?;
        if input.as_ref().is_some_and(|block| {
            block.header().format != request.fixture.format
                || block.header().frame_count != request.fixture.frames
        }) {
            return Err(INVALID);
        }
        let block = PcmBlock::prepare(
            request.fixture.format,
            request.block_frames,
            request.epochs,
            request.budget,
        )?;
        let output = VirtualOutput::prepare(request.fixture.format, frames, request.epochs)?;
        Ok(Self {
            request,
            source,
            input,
            block,
            output,
            events: Vec::with_capacity(events),
            decoded_frames: 0,
            offset: 0,
            fault_index: 0,
            write_limit: usize::MAX,
            processed: false,
        })
    }
    fn event(&mut self, event: RenderEventKind) {
        self.events.push(RenderEvent {
            at_frame: self.output.clock().frames(),
            epochs: self.request.epochs,
            event,
        });
    }
    fn read_block(&mut self) -> Result<usize, EngineError> {
        let frames = if let Some(input) = &self.input {
            let frames = (self.request.fixture.frames - self.decoded_frames)
                .min(self.request.block_frames as u64);
            if frames == 0 {
                return Ok(0);
            }
            self.block.set_span(
                self.request.epochs,
                self.decoded_frames / self.request.block_frames as u64,
                self.decoded_frames,
                frames,
            )?;
            let start = self
                .request
                .fixture
                .format
                .scalar_count(self.decoded_frames)?;
            for i in 0..self.request.fixture.format.scalar_count(frames)? {
                self.block
                    .set_integer_sample(i, input.integer_sample(start + i).ok_or(INVALID)?)?;
            }
            frames as usize
        } else {
            self.source.read_into(&mut self.block)?
        };
        self.decoded_frames += frames as u64;
        self.offset = 0;
        Ok(frames)
    }
    pub fn process(&mut self) -> Result<(), EngineError> {
        if self.processed {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "prepared render already consumed",
            ));
        }
        self.processed = true;
        self.event(RenderEventKind::Prepared);
        let result = self.process_blocks();
        match result {
            Ok(()) => self.event(RenderEventKind::Finished),
            Err(error) => self.event(RenderEventKind::Stopped { code: error.code }),
        }
        result
    }
    fn process_blocks(&mut self) -> Result<(), EngineError> {
        let mut available = 0;
        loop {
            if self.offset == available {
                available = self.read_block()?;
                if available == 0 {
                    return Ok(());
                }
            }
            let position = self.output.clock().frames();
            if let Some(scheduled) = self.request.faults.faults.get(self.fault_index).copied()
                && scheduled.at_frame == position
            {
                self.event(RenderEventKind::Fault {
                    fault: scheduled.fault,
                });
                self.fault_index += 1;
                match scheduled.fault {
                    FaultKind::ShortWrite { max_frames } => self.write_limit = max_frames,
                    FaultKind::ZeroProgress => self.write_limit = 0,
                    FaultKind::Underrun | FaultKind::BackendFailure => self.output.fail(),
                    FaultKind::MalformedChunk => {
                        return Err(PcmBlock::from_le_bytes(self.block.header(), &[0])
                            .err()
                            .unwrap_or(INVALID));
                    }
                    FaultKind::StaleEpoch => {
                        let mut header = self.block.header();
                        header.epochs.session = header.epochs.session.wrapping_add(1);
                        self.block.set_span(
                            header.epochs,
                            header.sequence,
                            header.frame_position,
                            header.frame_count,
                        )?;
                    }
                }
            }
            let until_fault = self
                .request
                .faults
                .faults
                .get(self.fault_index)
                .map_or(usize::MAX, |fault| (fault.at_frame - position) as usize);
            self.output
                .set_write_limit(self.write_limit.min(until_fault));
            let written = self.output.write(&self.block, self.offset)?;
            if written == 0 {
                return Err(EngineError::new(
                    ErrorCode::OutputFault,
                    "virtual backend made no progress",
                ));
            }
            self.offset += written;
            self.event(RenderEventKind::Written { frames: written });
        }
    }
    fn finish(self, result: Result<(), EngineError>) -> RenderReport {
        let frames = self.output.clock().frames();
        let elapsed_ns = self.output.clock().elapsed_ns();
        let format = self.request.fixture.format;
        let mut report = RenderReport {
            status: if result.is_ok() { "pass" } else { "fail" },
            error: result.err(),
            requested_format: format,
            resolved_format: Some(format),
            observed_format: Some(format),
            measurement_boundary: "engine_output",
            epochs: self.request.epochs,
            output_frames: frames,
            decoded_frames: self.decoded_frames,
            elapsed_ns,
            payload_sha256: String::new(),
            events: self.events,
            capture: self.output.into_capture(),
        };
        report.payload_sha256 = sha256(&report.capture_bytes());
        report
    }
}

pub fn render(request: RenderRequest) -> Result<RenderReport, RenderFailure> {
    render_input(request, None)
}
pub fn render_pcm(request: RenderRequest, input: PcmBlock) -> Result<RenderReport, RenderFailure> {
    render_input(request, Some(input))
}
fn render_input(
    request: RenderRequest,
    input: Option<PcmBlock>,
) -> Result<RenderReport, RenderFailure> {
    let format = request.fixture.format;
    let epochs = request.epochs;
    let mut prepared = match PreparedRender::prepare_input(request, input) {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(RenderFailure {
                error,
                report: Box::new(RenderReport::unprepared(format, epochs, error)),
            });
        }
    };
    let result = prepared.process();
    let report = prepared.finish(result);
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(RenderFailure {
            error,
            report: Box::new(report),
        }),
    }
}
