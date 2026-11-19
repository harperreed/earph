// ABOUTME: Prepares bounded independent-channel uniform partitioned convolution.
// ABOUTME: Owns its fixed FFT, working storage, stream latency, and exact FIR drain.
//! Arithmetic model: finite IEEE binary64, round-to-nearest, gradual underflow.
//! These numerical enclosures are not machine-checked formal proofs.
//!
//! The FFT is nine radix-two stages. Its roots use only basic arithmetic and sqrt.
//! Rust guarantees sqrt's rounded infinite-precision result:
//! <https://doc.rust-lang.org/std/primitive.f64.html#method.sqrt>.
//! Starting at c=0,s=1, seven half angles use c'=sqrt((1+c)/2), s'=s/(2*c').
//! With u=2^-53, c stays within 16u absolute error after seven steps.
//! Each division adds at most u relative error plus the cosine error divided by c'>=0.7.
//! The sine relative error stays below 256u. Each binary root thus has error below 512u.
//! A twiddle multiplies at most eight binary roots. Each complex multiply adds at most 16u.
//! Therefore its Euclidean error is below (1+529u)^8-1 < 8192u = 2^-40.
//! Each butterfly's local defect is below 2^-39 times its input magnitude sum.
//! Nine stages give a per-bin error below E*input_L1, where E=2^-34.
//! This exceeds (1+2^-39)^9-1, including complex multiply and butterfly addition rounding.
//! A separate floor 2^20*MIN_POSITIVE encloses accumulated per-operation underflow errors.
//!
//! For input peak A and tap L1 H, the exact spectral product sum is at most P*A*H.
//! Forward input FFT, tap FFT, and inverse FFT contribute a factor (1+E)^3.
//! Complex multiplication and K serial spectrum additions contribute 1+gamma(16K+32).
//! The final overlap addition contributes 1+gamma(4), where gamma(n)=nu/(1-nu).
//! Let d be the resulting factor minus one, rounded upward.
//! Two overlapping blocks give absolute error at most 2*P*A*H*d plus the underflow floor.
//! Thus runtime peak is bounded by A*H*(1+2*P*d) plus that explicit floor.
//! The floor is 2^40*MIN_POSITIVE*K*max(1,A)*max(1,H).
//! This exceeds propagation of all FFT, spectral sum, scaling, and overlap underflow errors.
//! There are no recursive arithmetic states: each overlap value survives one block only.
//! Standalone drain emits only the declared latency and authored FIR support.
//! Padded FFT blocks can carry rounding ghosts beyond that authored support.
//! Bounds use the authored binary64 taps, including zeros, without normalization.
//!
//! Numerical history clearance follows storage lifetimes, rather than authored support.
//! Number input blocks from zero. Let block b contain the last input before frame H.
//! If H>0, b=floor((H-1)/P), including an arbitrary partial-block phase.
//! A spectrum from block b last enters the partition sum for block b+K-1.
//! That sum's second half enters ready for block b+K, through overlap once.
//! Ready for block n emits at output frames (n+1)P through (n+2)P-1.
//! Therefore the last possibly contaminated output is before frame (b+K+2)P.
//! Since bP<H, H+(K+2)P safely exceeds that frontier for every phase.
//! At that frontier, pending and every retained input spectrum contain only input from H onward.
//! Transform and sum are overwritten on every block. No workspace residue survives their initialization.
//! Ready and overlap then depend only on those spectra and immutable filter spectra.
//! Consequently every later output obeys peak_bound(epsilon) when later input has magnitude <=epsilon.
//! This includes the explicit underflow floor when epsilon=0.
//! The claim assumes valid processing and the arithmetic model above, regardless of earlier admitted history.
//! H=0 needs no history removal; the same conservative horizon remains valid.
use std::mem::size_of;

pub const ALGORITHM_VERSION: &str = "earph-convolution-ola256-radix2-f64-v1";
pub const PARTITION_FRAMES: usize = 256;
pub const MAX_TAPS_PER_CHANNEL: usize = 262_144;
const FFT_FRAMES: usize = 512;
const MAX_CHANNELS: usize = 2;
const FFT_ERROR: f64 = 1.0 / ((1_u64 << 34) as f64);
const ROUND: f64 = f64::EPSILON / 2.0;
const FLOOR: f64 = f64::MIN_POSITIVE * ((1_u64 << 40) as f64);
// This also bounds the unscaled inverse FFT and all intermediate complex products.
const INTERMEDIATE_LIMIT: f64 = f64::MAX / (16.0 * FFT_FRAMES as f64 * PARTITION_FRAMES as f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvolutionError {
    Parameters,
    MemoryBudget,
    Span,
    NonFinite,
    InputPeak,
    Lifecycle,
    Numerical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvolutionResources {
    /// Includes the prepared object, fixed arrays, and Vec payload capacities.
    pub resident_bytes: u64,
    /// Includes resident storage and a conservative bounded preparation stack allowance.
    pub preparation_peak_bytes: u64,
    pub preparation_stack_bytes: u64,
    pub spectra_bytes: u64,
    pub history_bytes: u64,
    pub staging_bytes: u64,
    pub partition_count: usize,
    /// Bounds real adds, multiplies, and divisions. Excludes exact sign changes and routing.
    pub operations_per_block_per_channel: usize,
}

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f64,
    im: f64,
}
impl Complex {
    const ZERO: Self = Self { re: 0.0, im: 0.0 };
    const ONE: Self = Self { re: 1.0, im: 0.0 };
    fn multiply(self, rhs: Self) -> Self {
        Self {
            re: self.re * rhs.re - self.im * rhs.im,
            im: self.re * rhs.im + self.im * rhs.re,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Active,
    Draining,
    Finished,
    Cancelled,
    Faulted,
}

/// Allocation-free metadata for compiler admission, peak propagation, and finish scheduling.
#[derive(Debug, Clone, Copy)]
pub struct ConvolutionDesign {
    channels: usize,
    taps: usize,
    partitions: usize,
    max_quantum_frames: usize,
    tap_l1: [f64; MAX_CHANNELS],
    runtime_multiplier: f64,
    required_resources: ConvolutionResources,
}
impl ConvolutionDesign {
    /// Validate authored taps and derive bounds without FFT preparation or heap activity.
    pub fn analyze(taps: &[Vec<f64>], quantum: usize) -> Result<Self, ConvolutionError> {
        let channels = taps.len();
        if !(1..=MAX_CHANNELS).contains(&channels) || quantum == 0 {
            return Err(ConvolutionError::Parameters);
        }
        let tap_count = taps[0].len();
        if !(1..=MAX_TAPS_PER_CHANNEL).contains(&tap_count)
            || taps.iter().any(|channel| channel.len() != tap_count)
        {
            return Err(ConvolutionError::Parameters);
        }
        let mut tap_l1 = [0.0; MAX_CHANNELS];
        for (channel, values) in taps.iter().enumerate() {
            for value in values {
                if !value.is_finite() {
                    return Err(ConvolutionError::NonFinite);
                }
                tap_l1[channel] = add_up(tap_l1[channel], value.abs());
            }
            if !tap_l1[channel].is_finite() || tap_l1[channel] > INTERMEDIATE_LIMIT {
                return Err(ConvolutionError::InputPeak);
            }
        }
        let partitions = tap_count.div_ceil(PARTITION_FRAMES);
        let spectrum_len = channels * partitions * FFT_FRAMES;
        let stage_len = quantum
            .checked_mul(channels)
            .ok_or(ConvolutionError::MemoryBudget)?;
        let required_resources = resources(spectrum_len, spectrum_len, stage_len, partitions)?;
        let mut factor = 1.0;
        for _ in 0..3 {
            factor = multiply_up(factor, add_up(1.0, FFT_ERROR));
        }
        factor = multiply_up(factor, add_up(1.0, gamma(16 * partitions + 32)));
        factor = multiply_up(factor, add_up(1.0, gamma(4)));
        let defect = (factor - 1.0).next_up();
        let runtime_multiplier = add_up(1.0, multiply_up(2.0 * PARTITION_FRAMES as f64, defect));
        Ok(Self {
            channels,
            taps: tap_count,
            partitions,
            max_quantum_frames: quantum,
            tap_l1,
            runtime_multiplier,
            required_resources,
        })
    }
    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn taps_per_channel(&self) -> usize {
        self.taps
    }
    pub fn latency_frames(&self) -> usize {
        PARTITION_FRAMES
    }
    /// Standalone emitted EOF tail, excluding the empty-source exception.
    pub fn tail_frames(&self) -> usize {
        PARTITION_FRAMES + self.taps - 1
    }
    /// Internal numerical settlement horizon. It does not extend the standalone emitted EOF tail.
    pub fn history_clear_frames(&self) -> usize {
        (self.partitions + 2) * PARTITION_FRAMES
    }
    /// Required prepared storage. Preparation separately checks actual allocation capacities.
    pub fn resources(&self) -> ConvolutionResources {
        self.required_resources
    }
    pub fn tap_l1_bound(&self, channel: usize) -> Result<f64, ConvolutionError> {
        self.tap_l1
            .get(channel)
            .copied()
            .filter(|_| channel < self.channels)
            .ok_or(ConvolutionError::Parameters)
    }
    /// Includes FFT error, accumulation error, overlap rounding, and an underflow floor.
    pub fn peak_bound(&self, channel: usize, input_peak: f64) -> Result<f64, ConvolutionError> {
        let l1 = self.tap_l1_bound(channel)?;
        if !input_peak.is_finite()
            || input_peak < 0.0
            || input_peak > INTERMEDIATE_LIMIT / l1.max(1.0)
        {
            return Err(ConvolutionError::InputPeak);
        }
        let ideal = multiply_up(input_peak, l1);
        let floor = multiply_up(
            multiply_up(
                multiply_up(FLOOR, self.partitions as f64),
                input_peak.max(1.0),
            ),
            l1.max(1.0),
        );
        let bound = add_up(multiply_up(ideal, self.runtime_multiplier), floor);
        if bound.is_finite() {
            Ok(bound)
        } else {
            Err(ConvolutionError::InputPeak)
        }
    }
}

pub struct PreparedConvolution {
    design: ConvolutionDesign,
    roots: [Complex; PARTITION_FRAMES],
    spectra: Vec<Complex>,
    history: Vec<Complex>,
    stage: Vec<f64>,
    transform: [Complex; FFT_FRAMES],
    sum: [Complex; FFT_FRAMES],
    pending: [f64; PARTITION_FRAMES * MAX_CHANNELS],
    ready: [f64; PARTITION_FRAMES * MAX_CHANNELS],
    overlap: [f64; PARTITION_FRAMES * MAX_CHANNELS],
    position: usize,
    head: usize,
    input_frames: usize,
    drain_remaining: usize,
    state: State,
}

impl PreparedConvolution {
    pub fn history_clear_frames(&self) -> usize {
        self.design.history_clear_frames()
    }
    /// Prepare off the processing boundary. Caller-owned tap storage is counted by its owner.
    pub fn prepare(
        taps: &[Vec<f64>],
        max_quantum_frames: usize,
        byte_budget: u64,
    ) -> Result<Self, ConvolutionError> {
        let mut design = ConvolutionDesign::analyze(taps, max_quantum_frames)?;
        if design.resources().preparation_peak_bytes > byte_budget {
            return Err(ConvolutionError::MemoryBudget);
        }
        let partitions = design.partitions;
        let spectrum_len = design.channels * partitions * FFT_FRAMES;
        let stage_len = design.max_quantum_frames * design.channels;
        let spectra = zeroed_vec(spectrum_len)?;
        let history = zeroed_vec(spectrum_len)?;
        let stage = zeroed_vec(stage_len)?;
        let measured = resources(
            spectra.capacity(),
            history.capacity(),
            stage.capacity(),
            partitions,
        )?;
        if measured.preparation_peak_bytes > byte_budget {
            return Err(ConvolutionError::MemoryBudget);
        }
        design.required_resources = measured;
        let mut prepared = Self {
            design,
            roots: [Complex::ZERO; PARTITION_FRAMES],
            spectra,
            history,
            stage,
            transform: [Complex::ZERO; FFT_FRAMES],
            sum: [Complex::ZERO; FFT_FRAMES],
            pending: [0.0; PARTITION_FRAMES * MAX_CHANNELS],
            ready: [0.0; PARTITION_FRAMES * MAX_CHANNELS],
            overlap: [0.0; PARTITION_FRAMES * MAX_CHANNELS],
            position: 0,
            head: 0,
            input_frames: 0,
            drain_remaining: 0,
            state: State::Active,
        };
        prepare_roots(&mut prepared.roots);
        for (channel, values) in taps.iter().enumerate() {
            for partition in 0..partitions {
                prepared.transform.fill(Complex::ZERO);
                for (slot, value) in prepared.transform.iter_mut().zip(
                    values
                        .iter()
                        .skip(partition * PARTITION_FRAMES)
                        .take(PARTITION_FRAMES),
                ) {
                    slot.re = *value;
                }
                fft(&mut prepared.transform, &prepared.roots, false);
                if !prepared
                    .transform
                    .iter()
                    .all(|v| v.re.is_finite() && v.im.is_finite())
                {
                    return Err(ConvolutionError::Numerical);
                }
                let start = (channel * partitions + partition) * FFT_FRAMES;
                prepared.spectra[start..start + FFT_FRAMES].copy_from_slice(&prepared.transform);
            }
        }
        Ok(prepared)
    }

    pub fn channels(&self) -> usize {
        self.design.channels()
    }
    pub fn taps_per_channel(&self) -> usize {
        self.design.taps_per_channel()
    }
    pub fn latency_frames(&self) -> usize {
        self.design.latency_frames()
    }
    /// Extra frames after the final source frame. Empty sources produce no drain frames.
    pub fn tail_frames(&self) -> usize {
        self.design.tail_frames()
    }
    pub fn resources(&self) -> ConvolutionResources {
        self.design.resources()
    }

    pub fn tap_l1_bound(&self, channel: usize) -> Result<f64, ConvolutionError> {
        self.design.tap_l1_bound(channel)
    }

    /// Includes FFT error, accumulation error, overlap rounding, and an underflow floor.
    pub fn peak_bound(&self, channel: usize, input_peak: f64) -> Result<f64, ConvolutionError> {
        self.design.peak_bound(channel, input_peak)
    }

    /// Consume and produce equal interleaved spans. Validation errors leave state and output unchanged.
    pub fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<usize, ConvolutionError> {
        if self.state != State::Active {
            return Err(ConvolutionError::Lifecycle);
        }
        let frames = self.validate_span(output)?;
        if input.len() != output.len() {
            return Err(ConvolutionError::Span);
        }
        let total = self
            .input_frames
            .checked_add(frames)
            .filter(|count| count.checked_add(self.tail_frames()).is_some())
            .ok_or(ConvolutionError::Span)?;
        let mut peaks = [0.0_f64; MAX_CHANNELS];
        for frame in input.chunks_exact(self.design.channels) {
            for (channel, value) in frame.iter().enumerate() {
                if !value.is_finite() {
                    return Err(ConvolutionError::NonFinite);
                }
                peaks[channel] = peaks[channel].max(value.abs());
            }
        }
        for (channel, peak) in peaks.iter().enumerate().take(self.design.channels) {
            self.peak_bound(channel, *peak)?;
        }
        for frame in 0..frames {
            for channel in 0..self.design.channels {
                let index = self.position * self.design.channels + channel;
                self.stage[frame * self.design.channels + channel] = self.ready[index];
                self.pending[index] = input[frame * self.design.channels + channel];
            }
            if !self.advance() {
                return self.fault();
            }
        }
        self.input_frames = total;
        output.copy_from_slice(&self.stage[..output.len()]);
        Ok(frames)
    }

    pub fn begin_drain(&mut self) -> Result<(), ConvolutionError> {
        if self.state != State::Active {
            return Err(ConvolutionError::Lifecycle);
        }
        self.drain_remaining = if self.input_frames == 0 {
            0
        } else {
            self.tail_frames()
        };
        self.state = State::Draining;
        Ok(())
    }

    /// Return only valid tail frames. Slots after that count remain untouched.
    pub fn drain(&mut self, output: &mut [f64]) -> Result<usize, ConvolutionError> {
        if !matches!(self.state, State::Draining | State::Finished) {
            return Err(ConvolutionError::Lifecycle);
        }
        if output.is_empty() && self.drain_remaining != 0 {
            return Err(ConvolutionError::Span);
        }
        let frames = self.validate_span(output)?.min(self.drain_remaining);
        for frame in 0..frames {
            for channel in 0..self.design.channels {
                let index = self.position * self.design.channels + channel;
                self.stage[frame * self.design.channels + channel] = self.ready[index];
                self.pending[index] = 0.0;
            }
            if !self.advance() {
                return self.fault();
            }
        }
        self.drain_remaining -= frames;
        if self.drain_remaining == 0 {
            self.state = State::Finished;
        }
        let samples = frames * self.design.channels;
        output[..samples].copy_from_slice(&self.stage[..samples]);
        Ok(frames)
    }

    pub fn reset(&mut self) {
        self.history.fill(Complex::ZERO);
        self.pending.fill(0.0);
        self.ready.fill(0.0);
        self.overlap.fill(0.0);
        self.transform.fill(Complex::ZERO);
        self.sum.fill(Complex::ZERO);
        self.stage.fill(0.0);
        self.position = 0;
        self.head = 0;
        self.input_frames = 0;
        self.drain_remaining = 0;
        self.state = State::Active;
    }

    /// Clear signal history without releasing any prepared allocation. Reset permits reuse.
    pub fn cancel(&mut self) {
        self.reset();
        self.state = State::Cancelled;
    }

    fn validate_span(&self, output: &[f64]) -> Result<usize, ConvolutionError> {
        if !output.len().is_multiple_of(self.design.channels)
            || output.len() / self.design.channels > self.design.max_quantum_frames
        {
            return Err(ConvolutionError::Span);
        }
        Ok(output.len() / self.design.channels)
    }

    fn fault<T>(&mut self) -> Result<T, ConvolutionError> {
        self.state = State::Faulted;
        Err(ConvolutionError::Numerical)
    }

    fn advance(&mut self) -> bool {
        self.position += 1;
        if self.position != PARTITION_FRAMES {
            return true;
        }
        self.position = 0;
        for channel in 0..self.design.channels {
            self.transform.fill(Complex::ZERO);
            for frame in 0..PARTITION_FRAMES {
                self.transform[frame].re = self.pending[frame * self.design.channels + channel];
            }
            fft(&mut self.transform, &self.roots, false);
            let start = (channel * self.design.partitions + self.head) * FFT_FRAMES;
            self.history[start..start + FFT_FRAMES].copy_from_slice(&self.transform);
            self.sum.fill(Complex::ZERO);
            for partition in 0..self.design.partitions {
                let slot =
                    (self.head + self.design.partitions - partition) % self.design.partitions;
                let source = (channel * self.design.partitions + slot) * FFT_FRAMES;
                let filter = (channel * self.design.partitions + partition) * FFT_FRAMES;
                for bin in 0..FFT_FRAMES {
                    let product = self.history[source + bin].multiply(self.spectra[filter + bin]);
                    self.sum[bin].re += product.re;
                    self.sum[bin].im += product.im;
                }
            }
            fft(&mut self.sum, &self.roots, true);
            for frame in 0..PARTITION_FRAMES {
                let index = frame * self.design.channels + channel;
                let ready = self.sum[frame].re / FFT_FRAMES as f64 + self.overlap[index];
                let overlap = self.sum[frame + PARTITION_FRAMES].re / FFT_FRAMES as f64;
                if !ready.is_finite() || !overlap.is_finite() {
                    return false;
                }
                self.ready[index] = ready;
                self.overlap[index] = overlap;
            }
            if !self
                .sum
                .iter()
                .all(|v| v.re.is_finite() && v.im.is_finite())
            {
                return false;
            }
        }
        self.head = (self.head + 1) % self.design.partitions;
        true
    }
}

fn zeroed_vec<T: Copy + Default>(len: usize) -> Result<Vec<T>, ConvolutionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| ConvolutionError::MemoryBudget)?;
    values.resize(len, T::default());
    Ok(values)
}

fn resources(
    spectra: usize,
    history: usize,
    stage: usize,
    partitions: usize,
) -> Result<ConvolutionResources, ConvolutionError> {
    let bytes = |count: usize, size: usize| {
        count
            .checked_mul(size)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(ConvolutionError::MemoryBudget)
    };
    let spectra_bytes = bytes(spectra, size_of::<Complex>())?;
    let history_bytes = bytes(history, size_of::<Complex>())?;
    let staging_bytes = bytes(stage, size_of::<f64>())?;
    let resident_bytes = spectra_bytes
        .checked_add(history_bytes)
        .and_then(|n| n.checked_add(staging_bytes))
        .and_then(|n| n.checked_add(size_of::<PreparedConvolution>() as u64))
        .ok_or(ConvolutionError::MemoryBudget)?;
    // Reserve four object-sized construction slots plus scalar frames and callees.
    // This includes compiler-created copies of the fixed arrays during construction.
    // Recheck this compiler-dependent allowance when qualifying another compiler or target.
    // No planner, hidden heap tables, or additional preparation Vecs exist.
    let preparation_stack_bytes = 4 * size_of::<PreparedConvolution>() as u64 + 8192;
    let preparation_peak_bytes = resident_bytes
        .checked_add(preparation_stack_bytes)
        .ok_or(ConvolutionError::MemoryBudget)?;
    Ok(ConvolutionResources {
        resident_bytes,
        preparation_peak_bytes,
        preparation_stack_bytes,
        spectra_bytes,
        history_bytes,
        staging_bytes,
        partition_count: partitions,
        operations_per_block_per_channel: 46_080 + 4096 * partitions + 1280,
    })
}

fn add_up(a: f64, b: f64) -> f64 {
    if a == 0.0 {
        return b;
    }
    if b == 0.0 {
        return a;
    }
    (a + b).next_up()
}
fn multiply_up(a: f64, b: f64) -> f64 {
    if a == 0.0 || b == 0.0 {
        return 0.0;
    }
    (a * b).next_up()
}
fn gamma(operations: usize) -> f64 {
    let numerator = multiply_up(operations as f64, ROUND);
    (numerator / (1.0 - numerator).next_down()).next_up()
}

fn prepare_roots(roots: &mut [Complex; PARTITION_FRAMES]) {
    let mut cosine: f64 = 0.0;
    let mut sine: f64 = 1.0;
    let mut stride = PARTITION_FRAMES / 2;
    loop {
        roots[stride] = Complex {
            re: cosine,
            im: -sine,
        };
        if stride == 1 {
            break;
        }
        cosine = ((1.0 + cosine) / 2.0).sqrt();
        sine /= 2.0 * cosine;
        stride /= 2;
    }
    roots[0] = Complex::ONE;
    for index in 1..PARTITION_FRAMES {
        if index.is_power_of_two() {
            continue;
        }
        let mut value = Complex::ONE;
        let mut bits = index;
        while bits != 0 {
            let bit = 1 << bits.trailing_zeros();
            value = value.multiply(roots[bit]);
            bits ^= bit;
        }
        roots[index] = value;
    }
}

fn fft(values: &mut [Complex; FFT_FRAMES], roots: &[Complex; PARTITION_FRAMES], inverse: bool) {
    for index in 0_usize..FFT_FRAMES {
        let reversed = index.reverse_bits() >> (usize::BITS - 9);
        if index < reversed {
            values.swap(index, reversed);
        }
    }
    let mut width = 2;
    while width <= FFT_FRAMES {
        let half = width / 2;
        let step = FFT_FRAMES / width;
        for start in (0..FFT_FRAMES).step_by(width) {
            for offset in 0..half {
                let mut root = roots[offset * step];
                if inverse {
                    root.im = -root.im;
                }
                let even = values[start + offset];
                let odd = values[start + offset + half].multiply(root);
                values[start + offset] = Complex {
                    re: even.re + odd.re,
                    im: even.im + odd.im,
                };
                values[start + offset + half] = Complex {
                    re: even.re - odd.re,
                    im: even.im - odd.im,
                };
            }
        }
        width *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerical_fault_keeps_entire_span_private_and_stays_terminal() {
        let mut kernel = PreparedConvolution::prepare(&[vec![0.01; 257]], 300, 1 << 25).unwrap();
        // Inject a damaged retained spectrum to exercise an otherwise unreachable arithmetic fault.
        kernel.history[FFT_FRAMES].re = f64::NAN;
        let mut output = [73.0; 300];
        assert_eq!(
            kernel.process(&[0.2; 300], &mut output),
            Err(ConvolutionError::Numerical)
        );
        assert_eq!(output, [73.0; 300]);
        assert_eq!(
            kernel.process(&[0.0; 300], &mut output),
            Err(ConvolutionError::Lifecycle)
        );
        assert_eq!(kernel.begin_drain(), Err(ConvolutionError::Lifecycle));
        assert_eq!(kernel.drain(&mut output), Err(ConvolutionError::Lifecycle));
        kernel.reset();
        kernel.process(&[0.0; 300], &mut output).unwrap();
        assert!(output.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn numerical_fault_during_drain_keeps_entire_span_private() {
        let mut kernel = PreparedConvolution::prepare(&[vec![0.01; 257]], 300, 1 << 25).unwrap();
        kernel.process(&[0.2; 255], &mut [0.0; 255]).unwrap();
        kernel.begin_drain().unwrap();
        kernel.history[FFT_FRAMES].re = f64::NAN;
        let mut output = [73.0; 300];
        assert_eq!(kernel.drain(&mut output), Err(ConvolutionError::Numerical));
        assert_eq!(output, [73.0; 300]);
        assert_eq!(kernel.drain(&mut output), Err(ConvolutionError::Lifecycle));
    }
}
