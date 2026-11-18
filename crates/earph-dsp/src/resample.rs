// ABOUTME: Owns bounded f64 SRC staging, padding, frame accounting, and offline drift controls.
// ABOUTME: Preserves the pinned Rubato support frontier and admits full preparation costs.
//! Rubato 5.1.0, defaults disabled, IEEE binary64 round-to-nearest with gradual underflow.
//! One virtual zero precedes nonempty input. It remains source-domain padding across joins.
//! After the final guarded chunk, exactly ceil(2L/C)+1 zero calls preserve conservative support.
//! Returned counts determine output duration. Integer delay is not an exact group delay.
//! The finite rate matrix uses ratios 1/2, 147/160, 1, 160/147, and 2.
//! Including drift, reciprocal steps remain below 2.003 source frames.
//! The scheduler stops near C-L-1, within one reciprocal step and rounding slack.
//! Four neighboring phases span at most two integer starts and L+1 source samples.
//! For L>=256, unread support therefore stays inside the retained 2L history.
//! After ceil(2L/C) zero calls, the next call reads only zero history and input.
//! The source audit and real boundary scenarios support this conservative frontier.
//! No amplitude test decides EOF. No output is trimmed for nominal delay or duration.
use audioadapter_buffers::direct::SequentialSliceOfVecs;
use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
use rubato::sinc_interpolator::{SincInterpolator, make_interpolator};
use rubato::{
    Adjustable, Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};
use sha2::{Digest, Sha256};

pub const ALGORITHM_VERSION: &str = "earph-rubato-5.1.0-guarded-sinc-v1";
pub const UPSTREAM_COMMIT: &str = "f8711e2252a419a8f61d5dfe378c9f0da6b2ba7e";
const PHASES: usize = 256;
const RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];
const CHUNKS: [usize; 7] = [1, 2, 64, 128, 256, 512, 1024];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcError {
    RatePair,
    Geometry,
    Budget,
    Arithmetic,
    NonFinite,
    InputPeak,
    Numerical,
    Library,
    Lifecycle,
    Ratio,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SrcStep {
    pub consumed_frames: usize,
    pub produced_frames: usize,
    pub need_input: bool,
    pub need_output: bool,
    pub terminal: bool,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SrcStats {
    pub real_input_frames: u64,
    /// Includes the leading guard and every zero-filled library chunk.
    pub library_input_frames: u64,
    pub library_calls: u64,
    pub generated_frames: u64,
    pub emitted_frames: u64,
    pub guard_frames: u64,
    pub explicit_padding_input_frames: u64,
    /// Exact ceil(N*output_rate/input_rate), independent of historical ASRC commands.
    pub nominal_output_frames: u64,
    pub ratio_commands: u64,
    /// Ordered fingerprint of accepted ppm/ramp commands and their historical call positions.
    pub ratio_history_hash: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrcResources {
    /// Conservative requested allocation bytes; excludes allocator overhead and process RSS.
    pub resident_bytes: u64,
    /// Full simultaneous requested bytes, including private coefficient generation arrays.
    pub preparation_peak_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrcStatus {
    pub staged_input_frames: usize,
    pub queued_output_frames: usize,
    pub remaining_finish_calls: usize,
    pub remaining_finish_output_bound: u64,
    pub terminal: bool,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrcRatioState {
    pub current_ratio: f64,
    pub target_ratio: f64,
    pub requested_ppm: f64,
}
/// Finite sampled measurements, not a continuous-frequency mathematical bound.
/// Receipt: target/src-numeric/resampler-benchmarks.json and response-data.csv.
#[derive(Debug, Clone, Copy)]
pub struct SrcEnvelope {
    pub measured_maximum_passband_error_db: f64,
    pub declared_maximum_passband_error_db: f64,
    pub measured_minimum_stopband_rejection_db: f64,
    pub declared_minimum_stopband_rejection_db: f64,
    pub passband_edge_lower_nyquist_fraction: f64,
    pub stopband_start_lower_nyquist_fraction: f64,
    pub qualified_minimum_ppm: f64,
    pub qualified_maximum_ppm: f64,
}
#[derive(Debug, Clone, Copy)]
pub struct SrcMetadata {
    pub input_rate: u32,
    pub output_rate: u32,
    pub channels: ChannelLayout,
    pub chunk_frames: usize,
    pub filter_length: usize,
    pub phases: usize,
    pub cutoff: f32,
    pub drain_zero_calls: usize,
    pub max_output_frames: usize,
    /// Truncated upstream report. This is neither an exact phase nor an exact group delay.
    pub truncated_output_delay: usize,
    /// Guard duration is the exact rational guard_time_numerator / input_rate seconds.
    pub guard_time_numerator: u32,
    pub peak_gain: f64,
    pub underflow_allowance: f64,
    pub maximum_input_magnitude: f64,
    pub library_max_relative_ratio: f64,
    /// Cutoff before upstream applies the nominal downsampling ratio.
    pub pre_scaling_cutoff: f32,
    pub envelope: SrcEnvelope,
    pub resources: SrcResources,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Finishing,
    Finished,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy)]
struct SrcGeometry {
    input_rate: u32,
    output_rate: u32,
    channels: ChannelLayout,
    chunk_frames: usize,
    preset: SrcPreset,
    length: usize,
    output_bound: usize,
    resources: SrcResources,
}
impl SrcGeometry {
    fn new(
        input_rate: u32,
        output_rate: u32,
        channels: ChannelLayout,
        chunk_frames: usize,
        preset: SrcPreset,
    ) -> Result<Self, SrcError> {
        if !rate_pair_supported(input_rate, output_rate) {
            return Err(SrcError::RatePair);
        }
        if !CHUNKS.contains(&chunk_frames) {
            return Err(SrcError::Geometry);
        }
        let length = match preset {
            SrcPreset::Quality => 1024,
            SrcPreset::Responsive => 256,
        };
        let output_bound = u64::try_from(chunk_frames)
            .map_err(|_| SrcError::Arithmetic)?
            .checked_mul(u64::from(output_rate))
            .and_then(|n| n.checked_mul(1000))
            .ok_or(SrcError::Arithmetic)?
            .div_ceil(u64::from(input_rate) * 999)
            .checked_add(10)
            .ok_or(SrcError::Arithmetic)?;
        let output_bound = usize::try_from(output_bound).map_err(|_| SrcError::Arithmetic)?;
        let resources = resource_bounds(length, chunk_frames, channels.count(), output_bound)?;
        Ok(Self {
            input_rate,
            output_rate,
            channels,
            chunk_frames,
            preset,
            length,
            output_bound,
            resources,
        })
    }
}

/// Immutable coefficient facts resolved outside processing. Analysis allocates a temporary table.
#[derive(Debug, Clone, Copy)]
pub struct SrcDesign {
    geometry: SrcGeometry,
    cutoff: Option<f32>,
    peak_gain: f64,
    underflow_allowance: f64,
    maximum_input_magnitude: f64,
    coefficient_fingerprint: [u8; 32],
    parameter_fingerprint: [u8; 32],
}

pub struct PreparedSrc {
    library: Async<f64>,
    input: Vec<Vec<f64>>,
    output: Vec<Vec<f64>>,
    metadata: SrcMetadata,
    stats: SrcStats,
    staged: usize,
    pending_start: usize,
    pending_end: usize,
    final_chunk_done: bool,
    zero_calls_remaining: usize,
    state: State,
    target_ratio: f64,
    requested_ppm: f64,
}
impl SrcDesign {
    /// Check complete requested construction bytes without allocating or resolving coefficients.
    pub fn resource_requirements(
        input_rate: u32,
        output_rate: u32,
        channels: ChannelLayout,
        chunk_frames: usize,
        preset: SrcPreset,
    ) -> Result<SrcResources, SrcError> {
        Ok(SrcGeometry::new(input_rate, output_rate, channels, chunk_frames, preset)?.resources)
    }
    pub fn analyze(
        input_rate: u32,
        output_rate: u32,
        channels: ChannelLayout,
        chunk_frames: usize,
        preset: SrcPreset,
        byte_budget: u64,
    ) -> Result<Self, SrcError> {
        let geometry = SrcGeometry::new(input_rate, output_rate, channels, chunk_frames, preset)?;
        let SrcGeometry {
            length, resources, ..
        } = geometry;
        if resources.preparation_peak_bytes > byte_budget {
            return Err(SrcError::Budget);
        }
        let ratio = f64::from(output_rate) / f64::from(input_rate);
        // Scaling the automatic cutoff also scales its transition gap.
        // Quality uses effective length and the admitted minimum drift ratio.
        let cutoff = if preset == SrcPreset::Quality && output_rate <= input_rate {
            let effective =
                (length as u64 * u64::from(output_rate) / u64::from(input_rate)) as usize;
            Some(
                rubato::calculate_cutoff::<f32>(effective, WindowFunction::BlackmanHarris2)
                    * 0.999_f32,
            )
        } else {
            None
        };
        // This temporary immutable table never overlaps Async's coefficient construction.
        let mut coefficients = Sha256::new();
        let (peak_gain, underflow_allowance, maximum_input_magnitude) = {
            let table = make_interpolator::<f64>(
                length,
                ratio,
                cutoff,
                PHASES,
                WindowFunction::BlackmanHarris2,
            );
            let mut maximum = 0.0_f64;
            for row in table.get_sincs() {
                let mut sum = 0.0_f64;
                for value in row.iter() {
                    if !value.is_finite() {
                        return Err(SrcError::Numerical);
                    }
                    sum = (sum + value.abs()).next_up();
                    coefficients.update(value.to_bits().to_le_bytes());
                }
                maximum = maximum.max(sum);
            }
            // Cubic Lagrange weights have L1 <= 1+x*(1-x) <= 1.25 for every phase.
            // Expanded polynomial absolute terms sum to <= (19/3)*maximum.
            let operations = (16 * length + 128) as f64;
            let nu = (operations * (f64::EPSILON / 2.0)).next_up();
            let gamma = (nu / (1.0 - nu).next_down()).next_up();
            let terms = ((19.0_f64 / 3.0).next_up() * maximum).next_up();
            let rounding = (terms * gamma).next_up();
            let gain = ((1.25 * maximum).next_up() + rounding).next_up().next_up();
            let floor = (operations * 32.0 * f64::MIN_POSITIVE).next_up();
            // Internal polynomial terms can exceed the final output bound before cancellation.
            let input_limit = (f64::MAX / (32.0 * maximum).next_up()).next_down();
            if !gain.is_finite() || !input_limit.is_finite() || gain < 1.0 {
                return Err(SrcError::Numerical);
            }
            (gain, floor, input_limit)
        };
        let coefficient_fingerprint: [u8; 32] = coefficients.finalize().into();
        let mut parameters = Sha256::new();
        parameters.update(ALGORITHM_VERSION.as_bytes());
        parameters.update(UPSTREAM_COMMIT.as_bytes());
        for word in [
            u64::from(input_rate),
            u64::from(output_rate),
            channels.count() as u64,
            chunk_frames as u64,
            length as u64,
            PHASES as u64,
            u64::from(
                cutoff
                    .unwrap_or_else(|| {
                        rubato::calculate_cutoff::<f32>(length, WindowFunction::BlackmanHarris2)
                    })
                    .to_bits(),
            ),
            u64::from(cutoff.is_some()),
        ] {
            parameters.update(word.to_le_bytes());
        }
        parameters.update(coefficient_fingerprint);
        Ok(Self {
            geometry,
            cutoff,
            peak_gain,
            underflow_allowance,
            maximum_input_magnitude,
            coefficient_fingerprint,
            parameter_fingerprint: parameters.finalize().into(),
        })
    }
    pub fn input_rate(&self) -> u32 {
        self.geometry.input_rate
    }
    pub fn output_rate(&self) -> u32 {
        self.geometry.output_rate
    }
    pub fn channels(&self) -> ChannelLayout {
        self.geometry.channels
    }
    pub fn chunk_frames(&self) -> usize {
        self.geometry.chunk_frames
    }
    pub fn max_output_frames(&self) -> usize {
        self.geometry.output_bound
    }
    pub fn resources(&self) -> SrcResources {
        self.geometry.resources
    }
    pub fn coefficient_fingerprint(&self) -> [u8; 32] {
        self.coefficient_fingerprint
    }
    pub fn parameter_fingerprint(&self) -> [u8; 32] {
        self.parameter_fingerprint
    }
    pub fn peak_bound(&self, input_peak: f64) -> Result<f64, SrcError> {
        if !input_peak.is_finite() || input_peak < 0.0 {
            return Err(SrcError::NonFinite);
        }
        if input_peak > self.maximum_input_magnitude {
            return Err(SrcError::InputPeak);
        }
        let bound = ((self.peak_gain * input_peak).next_up() + self.underflow_allowance).next_up();
        if bound.is_finite() {
            Ok(bound)
        } else {
            Err(SrcError::Numerical)
        }
    }
    pub fn instantiate(self, byte_budget: u64) -> Result<PreparedSrc, SrcError> {
        let SrcGeometry {
            input_rate,
            output_rate,
            channels,
            chunk_frames,
            preset,
            length,
            output_bound,
            resources,
        } = self.geometry;
        if resources.preparation_peak_bytes > byte_budget {
            return Err(SrcError::Budget);
        }
        let Self {
            cutoff,
            peak_gain,
            underflow_allowance,
            maximum_input_magnitude,
            ..
        } = self;
        let ratio = f64::from(output_rate) / f64::from(input_rate);
        let h = channels.count();
        let mut parameters =
            SincInterpolationParameters::new(length, WindowFunction::BlackmanHarris2)
                .oversampling_factor(PHASES)
                .interpolation(SincInterpolationType::Cubic);
        if let Some(cutoff) = cutoff {
            parameters = parameters.f_cutoff(cutoff);
        }
        let library = Async::<f64>::new_sinc(
            ratio,
            1.0 / 0.999,
            &parameters,
            chunk_frames,
            h,
            FixedAsync::Input,
        )
        .map_err(|_| SrcError::Library)?;
        if library.output_frames_max() > output_bound || library.input_frames_max() != chunk_frames
        {
            return Err(SrcError::Geometry);
        }
        let metadata = SrcMetadata {
            input_rate,
            output_rate,
            channels,
            chunk_frames,
            filter_length: length,
            phases: PHASES,
            cutoff: library.cutoff().ok_or(SrcError::Library)?,
            drain_zero_calls: (2 * length).div_ceil(chunk_frames) + 1,
            max_output_frames: output_bound,
            truncated_output_delay: library.output_delay(),
            guard_time_numerator: 1,
            peak_gain,
            underflow_allowance,
            maximum_input_magnitude,
            library_max_relative_ratio: 1.0 / 0.999,
            pre_scaling_cutoff: cutoff.unwrap_or_else(|| {
                rubato::calculate_cutoff::<f32>(length, WindowFunction::BlackmanHarris2)
            }),
            envelope: measured_envelope(input_rate, output_rate, preset),
            resources,
        };
        Ok(PreparedSrc {
            library,
            input: vec![vec![0.0; chunk_frames]; h],
            output: vec![vec![0.0; output_bound]; h],
            metadata,
            stats: SrcStats::default(),
            staged: 0,
            pending_start: 0,
            pending_end: 0,
            final_chunk_done: false,
            zero_calls_remaining: 0,
            state: State::Running,
            target_ratio: ratio,
            requested_ppm: 0.0,
        })
    }
}
impl PreparedSrc {
    pub fn prepare(
        input_rate: u32,
        output_rate: u32,
        channels: ChannelLayout,
        chunk_frames: usize,
        preset: SrcPreset,
        byte_budget: u64,
    ) -> Result<Self, SrcError> {
        SrcDesign::analyze(
            input_rate,
            output_rate,
            channels,
            chunk_frames,
            preset,
            byte_budget,
        )?
        .instantiate(byte_budget)
    }
    pub fn metadata(&self) -> SrcMetadata {
        self.metadata
    }
    pub fn stats(&self) -> SrcStats {
        self.stats
    }
    pub fn ratio_state(&self) -> SrcRatioState {
        SrcRatioState {
            current_ratio: self.library.resample_ratio(),
            target_ratio: self.target_ratio,
            requested_ppm: self.requested_ppm,
        }
    }
    pub fn status(&self) -> SrcStatus {
        let terminal = matches!(
            self.state,
            State::Finished | State::Cancelled | State::Failed
        );
        let calls = if terminal || self.stats.real_input_frames == 0 {
            0
        } else if self.state == State::Running {
            usize::from(self.staged > 0) + self.metadata.drain_zero_calls
        } else {
            usize::from(!self.final_chunk_done) + self.zero_calls_remaining
        };
        let queued = self.pending_end - self.pending_start;
        // Admitted geometry limits this to fewer than 2^24 frames.
        SrcStatus {
            staged_input_frames: self.staged,
            queued_output_frames: queued,
            remaining_finish_calls: calls,
            remaining_finish_output_bound: queued as u64
                + calls as u64 * self.metadata.max_output_frames as u64,
            terminal,
        }
    }
    /// This serial control belongs to offline qualification, never a live device clock loop.
    /// Each accepted command is anchored to cumulative public library and source counts.
    pub fn set_offline_ratio_ppm(&mut self, ppm: f64, ramp: bool) -> Result<(), SrcError> {
        if self.state != State::Running {
            return Err(SrcError::Lifecycle);
        }
        if !ppm.is_finite() || !(-1000.0..=1000.0).contains(&ppm) {
            return Err(SrcError::Ratio);
        }
        let commands = self
            .stats
            .ratio_commands
            .checked_add(1)
            .ok_or(SrcError::Arithmetic)?;
        self.library
            .set_resample_ratio_relative(1.0 + ppm / 1_000_000.0, ramp)
            .map_err(|_| SrcError::Ratio)?;
        let mut hash = self.stats.ratio_history_hash;
        for word in [
            ppm.to_bits(),
            u64::from(ramp),
            self.stats.library_calls,
            self.stats.real_input_frames,
            self.stats.generated_frames,
        ] {
            hash = hash.rotate_left(7) ^ word;
        }
        self.stats.ratio_commands = commands;
        self.target_ratio = f64::from(self.metadata.output_rate)
            / f64::from(self.metadata.input_rate)
            * (1.0 + ppm / 1_000_000.0);
        self.requested_ppm = ppm;
        self.stats.ratio_history_hash = hash;
        Ok(())
    }
    pub fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<SrcStep, SrcError> {
        let result = self.process_inner(input, output);
        self.publish_result(result, output)
    }
    fn process_inner(&mut self, input: &[f64], output: &mut [f64]) -> Result<SrcStep, SrcError> {
        if self.state != State::Running {
            return Err(SrcError::Lifecycle);
        }
        let h = self.metadata.channels.count();
        if !input.len().is_multiple_of(h) || !output.len().is_multiple_of(h) {
            return Err(SrcError::Geometry);
        }
        if self.pending_start < self.pending_end {
            return self.emit(output, 0);
        }
        let capacity = self.metadata.chunk_frames;
        let start_stream = self.stats.guard_frames == 0 && !input.is_empty();
        let available = capacity - self.staged - usize::from(start_stream);
        // C1 first processes the guard, then accepts the first real frame into staging.
        let accepted = (input.len() / h).min(if start_stream && capacity == 1 {
            1
        } else {
            available
        });
        let selected = &input[..accepted * h];
        if selected.iter().any(|v| !v.is_finite()) {
            return Err(SrcError::NonFinite);
        }
        if selected
            .iter()
            .any(|v| v.abs() > self.metadata.maximum_input_magnitude)
        {
            return Err(SrcError::InputPeak);
        }
        let real = self
            .stats
            .real_input_frames
            .checked_add(accepted as u64)
            .ok_or(SrcError::Arithmetic)?;
        let nominal = u128::from(real)
            .checked_mul(u128::from(self.metadata.output_rate))
            .ok_or(SrcError::Arithmetic)?
            .div_ceil(u128::from(self.metadata.input_rate));
        let nominal = u64::try_from(nominal).map_err(|_| SrcError::Arithmetic)?;
        if start_stream {
            for channel in &mut self.input {
                channel[0] = 0.0;
            }
            self.staged = 1;
            self.stats.guard_frames = 1;
        }
        if start_stream && capacity == 1 {
            self.call_library(1)?;
        }
        for (frame, source) in selected.chunks_exact(h).enumerate() {
            for (channel, value) in source.iter().enumerate() {
                self.input[channel][self.staged + frame] = *value;
            }
        }
        self.staged += accepted;
        self.stats.real_input_frames = real;
        self.stats.nominal_output_frames = nominal;
        if self.staged == capacity && !(start_stream && capacity == 1) {
            self.call_library(capacity)?;
        }
        self.emit(output, accepted)
    }
    pub fn begin_finish(&mut self) -> Result<(), SrcError> {
        if self.state != State::Running {
            return Err(SrcError::Lifecycle);
        }
        self.state = State::Finishing;
        self.final_chunk_done = self.staged == 0;
        self.zero_calls_remaining = if self.stats.real_input_frames == 0 {
            0
        } else {
            self.metadata.drain_zero_calls
        };
        Ok(())
    }
    pub fn finish(&mut self, output: &mut [f64]) -> Result<SrcStep, SrcError> {
        let result = self.finish_inner(output);
        self.publish_result(result, output)
    }
    fn finish_inner(&mut self, output: &mut [f64]) -> Result<SrcStep, SrcError> {
        if !output.len().is_multiple_of(self.metadata.channels.count()) {
            return Err(SrcError::Geometry);
        }
        if self.state == State::Finished {
            return Ok(SrcStep {
                terminal: true,
                ..SrcStep::default()
            });
        }
        if self.state != State::Finishing {
            return Err(SrcError::Lifecycle);
        }
        if self.pending_start == self.pending_end {
            if !self.final_chunk_done {
                self.call_library(self.staged)?;
                self.final_chunk_done = true;
            } else if self.zero_calls_remaining > 0 {
                self.call_library(0)?;
                self.zero_calls_remaining -= 1;
            }
        }
        if self.final_chunk_done
            && self.zero_calls_remaining == 0
            && self.pending_start == self.pending_end
        {
            self.state = State::Finished;
        }
        self.emit(output, 0)
    }
    fn call_library(&mut self, valid: usize) -> Result<(), SrcError> {
        if self.pending_start != self.pending_end || valid > self.metadata.chunk_frames {
            return Err(SrcError::Geometry);
        }
        let expected = self.library.output_frames_next();
        if expected > self.metadata.max_output_frames
            || self.library.input_frames_next() != self.metadata.chunk_frames
        {
            return Err(SrcError::Geometry);
        }
        let calls = self
            .stats
            .library_calls
            .checked_add(1)
            .ok_or(SrcError::Arithmetic)?;
        let consumed = self
            .stats
            .library_input_frames
            .checked_add(self.metadata.chunk_frames as u64)
            .ok_or(SrcError::Arithmetic)?;
        self.stats
            .generated_frames
            .checked_add(expected as u64)
            .ok_or(SrcError::Arithmetic)?;
        let h = self.metadata.channels.count();
        let input = SequentialSliceOfVecs::new(&self.input, h, self.metadata.chunk_frames)
            .map_err(|_| SrcError::Geometry)?;
        let mut output =
            SequentialSliceOfVecs::new_mut(&mut self.output, h, self.metadata.max_output_frames)
                .map_err(|_| SrcError::Geometry)?;
        let indexing = Indexing::new().partial_len(valid);
        let (read, written) = self
            .library
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|_| SrcError::Library)?;
        if read != self.metadata.chunk_frames || written != expected {
            return Err(SrcError::Library);
        }
        if self
            .output
            .iter()
            .any(|channel| channel[..written].iter().any(|sample| !sample.is_finite()))
        {
            return Err(SrcError::Numerical);
        }
        self.stats.library_calls = calls;
        self.stats.library_input_frames = consumed;
        self.stats.generated_frames += written as u64;
        self.staged = 0;
        self.pending_start = 0;
        self.pending_end = written;
        Ok(())
    }
    fn emit(&mut self, output: &mut [f64], consumed: usize) -> Result<SrcStep, SrcError> {
        let h = self.metadata.channels.count();
        let frames = (output.len() / h).min(self.pending_end - self.pending_start);
        let emitted = self
            .stats
            .emitted_frames
            .checked_add(frames as u64)
            .ok_or(SrcError::Arithmetic)?;
        // The total may exceed u64 while its small padding difference still fits.
        let padding = (u128::from(self.stats.library_input_frames) + self.staged as u128)
            .checked_sub(u128::from(self.stats.real_input_frames))
            .and_then(|difference| u64::try_from(difference).ok())
            .ok_or(SrcError::Arithmetic)?;
        for (frame, destination) in output[..frames * h].chunks_exact_mut(h).enumerate() {
            for (channel, value) in destination.iter_mut().enumerate() {
                *value = self.output[channel][self.pending_start + frame];
            }
        }
        self.pending_start += frames;
        self.stats.emitted_frames = emitted;
        // Padded input includes staged guard/real samples, even before their library call.
        self.stats.explicit_padding_input_frames = padding;
        if self.state == State::Finishing
            && self.final_chunk_done
            && self.zero_calls_remaining == 0
            && self.pending_start == self.pending_end
        {
            self.state = State::Finished;
        }
        Ok(SrcStep {
            consumed_frames: consumed,
            produced_frames: frames,
            need_input: self.state == State::Running
                && self.pending_start == self.pending_end
                && self.staged < self.metadata.chunk_frames,
            need_output: self.pending_start < self.pending_end,
            terminal: self.state == State::Finished,
        })
    }
    fn publish_result(
        &mut self,
        result: Result<SrcStep, SrcError>,
        output: &mut [f64],
    ) -> Result<SrcStep, SrcError> {
        if result.is_err() {
            output.fill(0.0);
            self.input.iter_mut().for_each(|channel| channel.fill(0.0));
            self.output.iter_mut().for_each(|channel| channel.fill(0.0));
            self.staged = 0;
            self.pending_start = 0;
            self.pending_end = 0;
            self.state = State::Failed;
        }
        result
    }
    pub fn reset(&mut self) {
        self.library.reset();
        self.input.iter_mut().for_each(|channel| channel.fill(0.0));
        self.output.iter_mut().for_each(|channel| channel.fill(0.0));
        self.stats = SrcStats::default();
        self.staged = 0;
        self.pending_start = 0;
        self.pending_end = 0;
        self.final_chunk_done = false;
        self.zero_calls_remaining = 0;
        self.state = State::Running;
        self.target_ratio =
            f64::from(self.metadata.output_rate) / f64::from(self.metadata.input_rate);
        self.requested_ppm = 0.0;
    }
    pub fn cancel(&mut self) {
        self.reset();
        self.state = State::Cancelled;
    }
}
/// Only these finite nominal pairs share a qualified source-support and numeric envelope.
pub fn rate_pair_supported(input: u32, output: u32) -> bool {
    RATES.contains(&input)
        && RATES.contains(&output)
        && (input == output
            || input.checked_mul(2) == Some(output)
            || output.checked_mul(2) == Some(input)
            || u64::from(input) * 160 == u64::from(output) * 147
            || u64::from(input) * 147 == u64::from(output) * 160)
}
fn resource_bounds(l: usize, c: usize, h: usize, o: usize) -> Result<SrcResources, SrcError> {
    // u128 avoids intermediate machine-size overflow. Geometry is admitted before allocation.
    let (l, c, h, o, f) = (l as u128, c as u128, h as u128, o as u128, PHASES as u128);
    let resident = 8 * l * f + 24 * f + 8 * h * (c + 2 * l) + 25 * h + 8 * (l + 1) + 1024;
    let peak = resident.max(24 * l * f + 64 * f + 1024);
    let adapter = 8 * h * (c + o) + 48 * h + 48 + std::mem::size_of::<PreparedSrc>() as u128;
    Ok(SrcResources {
        resident_bytes: u64::try_from(resident + adapter).map_err(|_| SrcError::Arithmetic)?,
        preparation_peak_bytes: u64::try_from(peak + adapter).map_err(|_| SrcError::Arithmetic)?,
    })
}

fn measured_envelope(input: u32, output: u32, preset: SrcPreset) -> SrcEnvelope {
    // Each rate family shares the same binary64 nominal ratio and normalized frequency grid.
    // These observations cover -1000, 0, and +1000 ppm with the final fixed coefficients.
    let (pass, stop) = match (preset, input.cmp(&output)) {
        (SrcPreset::Quality, std::cmp::Ordering::Equal) => {
            (2.035226618983685e-9, 115.57591115807017)
        }
        (SrcPreset::Responsive, std::cmp::Ordering::Equal) => {
            (0.00006089620503684288, 101.86946113901817)
        }
        (SrcPreset::Quality, _) if input == 2 * output => {
            (8.287594183623642e-10, 142.75025790895248)
        }
        (SrcPreset::Responsive, _) if input == 2 * output => {
            (0.22569342989413876, 127.25320102989977)
        }
        (SrcPreset::Quality, _) if output == 2 * input => {
            (2.0158822100001154e-9, 214.39065846843422)
        }
        (SrcPreset::Responsive, _) if output == 2 * input => {
            (0.00006089620601564212, 183.00118056486767)
        }
        (SrcPreset::Quality, std::cmp::Ordering::Greater) => {
            (1.4814770004428252e-9, 146.14539437072008)
        }
        (SrcPreset::Responsive, std::cmp::Ordering::Greater) => {
            (0.0006144307201195392, 106.50542684126778)
        }
        (SrcPreset::Quality, _) => (2.0153846170272585e-9, 168.82350404065122),
        (SrcPreset::Responsive, _) => (0.00006089620575430754, 146.45930679999626),
    };
    let half_responsive = preset == SrcPreset::Responsive && input == 2 * output;
    SrcEnvelope {
        measured_maximum_passband_error_db: pass,
        declared_maximum_passband_error_db: if half_responsive { 0.25 } else { 0.01 },
        measured_minimum_stopband_rejection_db: stop,
        declared_minimum_stopband_rejection_db: 100.0,
        passband_edge_lower_nyquist_fraction: 0.9,
        stopband_start_lower_nyquist_fraction: if half_responsive { 1.05 } else { 1.0 },
        qualified_minimum_ppm: -1000.0,
        qualified_maximum_ppm: 1000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_count_keeps_exact_differences_near_the_counter_limit() {
        let mut kernel = PreparedSrc::prepare(
            48000,
            44100,
            ChannelLayout::Mono,
            64,
            SrcPreset::Responsive,
            16 << 20,
        )
        .unwrap();
        kernel.stats.library_input_frames = u64::MAX - 2;
        kernel.stats.real_input_frames = u64::MAX - 3;
        kernel.staged = 3;
        kernel.emit(&mut [], 0).unwrap();
        assert_eq!(kernel.stats.explicit_padding_input_frames, 4);
    }

    #[test]
    fn half_rate_quality_coefficients_preserve_the_declared_drift_edges() {
        let kernel = PreparedSrc::prepare(
            96000,
            48000,
            ChannelLayout::Mono,
            128,
            SrcPreset::Quality,
            16 << 20,
        )
        .unwrap();
        let metadata = kernel.metadata();
        let table = make_interpolator::<f64>(
            metadata.filter_length,
            0.5,
            Some(metadata.pre_scaling_cutoff),
            PHASES,
            WindowFunction::BlackmanHarris2,
        );
        for row in table.get_sincs().iter().step_by(16) {
            for (frequency, passband) in [(0.9 * 0.4995 / 2.0, true), (0.4995 / 2.0, false)] {
                let (re, im) = row
                    .iter()
                    .enumerate()
                    .fold((0.0, 0.0), |(re, im), (i, value)| {
                        let angle = std::f64::consts::TAU * frequency * i as f64;
                        (re + value * angle.cos(), im + value * angle.sin())
                    });
                if passband {
                    assert!((20.0 * re.hypot(im).log10()).abs() <= 0.01);
                } else {
                    assert!(re.hypot(im) <= 1e-5);
                }
            }
        }
    }
}
