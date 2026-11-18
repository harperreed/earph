// ABOUTME: Prepares bounded cookbook PEQ sections and independent f64 channel state.
// ABOUTME: Encloses modal decay and runtime rounding before choosing finite IIR truncation.
//! Arithmetic model: finite IEEE binary64, round-to-nearest, gradual underflow.
//! Coefficient generation chooses binary64 values. Bounds apply to those chosen values.
//! The response tests separately compare them with the cookbook's ideal transfer function.
//!
//! Let `s' = M s + B x`, `y = b0*x + [1,0]s`, and `s = T z`.
//! Preparation encloses `R=T^-1 M T` and `C=T^-1 B` with outward rounding.
//! A rotation or symmetric reference plus its enclosed defect bounds `||R||2` by `q`.
//! Thus the exact impulse sum is at most `|b0| + ||C||2/(1-q)`.
//! Distinct real poles also admit separate geometric sums of enclosed impulse residues.
//! Taking the smaller complete bound preserves the upper-bound direction.
//!
//! Each runtime update has transformed error at most `eta_s*||z||2 + eta_x*|x| + f`.
//! The formulas include output rounding in both feedback updates.
//! Write `Z=||z||2`, `rho=||[-alpha,beta]||2`, and `g=ROUND`.
//! Output error is at most `e_y=g*(|b0|*P+Z)+2*TINY`.
//! The output magnitude is at most `Y=(1+g)*(|b0|*P+Z)+2*TINY`.
//! First-state error is at most `|a1|*e_y+g*(|b1|*P+|a1|*Y+rho*Z)+8*TINY`.
//! Second-state error is at most `|a2|*e_y+g*(|b2|*P+|a2|*Y)+8*TINY`.
//! Multiplying their Euclidean norm by `||T^-1||F` bounds the transformed update error.
//! Consequently `||z'||2 <= q_runtime*||z||2 + c_runtime*P + f` for `|x|<=P`.
//! Starting from zero, state never exceeds `(c_runtime*P+f)/(1-q_runtime)`.
//! Subtracting the exact recurrence bounds runtime error using the original contraction `q`.
//! This separates the tight exact impulse sum from the conservative numerical allowance.
//!
//! After `T` zero-input frames, state is at most `q_runtime^T*state_bound + f/(1-q_runtime)`.
//! The next output and every later output satisfy the published residual bound.
//! The horizon counts emitted zero-input frames. It excludes the last input frame.
//! Upstream bounded history is already included through `state_bound`.
//! Downstream amplification is supplied explicitly when selecting the horizon.
//! These are numerical enclosures under this model, not machine-checked formal proofs.
use earph_core::{audio::ChannelLayout, dsp_nodes::PeqFilter};

pub const ALGORITHM_VERSION: &str = "earph-peq-rbj-tdf2-modal-interval-v1";
pub const MAX_TAIL_FRAMES: usize = 1_048_576;
const JURY_MARGIN: f64 = 1e-10;
// Bounds cover separate multiplies and adds. Fused operations only reduce this allowance.
const ROUND: f64 = 8.0 * f64::EPSILON;
// Proof arithmetic assumes IEEE round-to-nearest with gradual underflow.
// This deliberately loose allowance exceeds each operation's underflow error.
const TINY: f64 = f64::MIN_POSITIVE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeqError {
    Parameters,
    Stability,
    ProofBudget,
    TailBudget,
    Span,
    NonFinite,
    InputPeak,
    Lifecycle,
    Numerical,
}

#[derive(Debug, Clone, Copy)]
pub struct PeqResources {
    pub resident_bytes: usize,
    pub preparation_scratch_bytes: usize,
    pub state_scalars: usize,
    /// Multiplies and additions in the section recurrence; excludes validation and routing.
    pub filter_operations_per_frame: usize,
    pub proof_matrix_products: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PeqDesign {
    coefficients: [f64; 5],
    channels: ChannelLayout,
    ideal_decay: f64,
    ideal_drive: f64,
    ideal_l1: f64,
    runtime_decay: f64,
    runtime_drive: f64,
    runtime_floor: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeqTail {
    coefficient_bits: [u64; 5],
    channels: ChannelLayout,
    input_peak: f64,
    frames: usize,
    discarded_output_bound: f64,
}
impl PeqTail {
    pub fn frames(self) -> usize {
        self.frames
    }
    /// Includes the downstream gain supplied during tail selection.
    pub fn discarded_output_bound(self) -> f64 {
        self.discarded_output_bound
    }
}

impl PeqDesign {
    pub fn prepare(
        filter: &PeqFilter,
        sample_rate_hz: u32,
        channels: ChannelLayout,
    ) -> Result<Self, PeqError> {
        filter
            .validate(sample_rate_hz)
            .map_err(|_| PeqError::Parameters)?;
        Self::from_coefficients(cookbook(filter, sample_rate_hz)?, channels)
    }

    fn from_coefficients(
        coefficients: [f64; 5],
        channels: ChannelLayout,
    ) -> Result<Self, PeqError> {
        if !coefficients.iter().all(|x| x.is_finite()) {
            return Err(PeqError::NonFinite);
        }
        let [b0, b1, b2, a1, a2] = coefficients.map(Interval::point);
        for expression in [
            Interval::point(1.0).add(a1).add(a2),
            Interval::point(1.0).sub(a1).add(a2),
            Interval::point(1.0).sub(a2),
        ] {
            if expression.lo <= JURY_MARGIN {
                return Err(PeqError::Stability);
            }
        }
        if coefficients == [1.0, 0.0, 0.0, 0.0, 0.0] {
            return Ok(Self {
                coefficients,
                channels,
                ideal_decay: 0.0,
                ideal_drive: 0.0,
                ideal_l1: 1.0,
                runtime_decay: 0.0,
                runtime_drive: 0.0,
                runtime_floor: 0.0,
            });
        }

        // s=Tz, T=[[1,0],[-alpha,beta]]. The exact transformed matrix is enclosed below.
        // Complex poles approach a rotation. Real poles approach a symmetric matrix.
        // A positive beta floor also handles repeated and nearly repeated poles.
        let alpha = -coefficients[3] / 2.0;
        let discriminant = coefficients[4] - alpha * alpha;
        let beta = discriminant.abs().sqrt().max((1.0 - alpha.abs()) * 0.125);
        if !beta.is_finite() || beta <= 0.0 {
            return Err(PeqError::ProofBudget);
        }
        let alpha = Interval::point(alpha);
        let beta = Interval::point(beta);
        let zero = Interval::point(0.0);
        let one = Interval::point(1.0);
        let matrix = [[a1.neg(), one], [a2.neg(), zero]];
        let transform = [[one, zero], [alpha.neg(), beta]];
        let inverse = [[one, zero], [alpha.div(beta), one.div(beta)]];
        let transformed = product(product(inverse, matrix), transform);
        let complex = discriminant >= 0.0;
        let reference = [
            [alpha, beta],
            [if complex { beta.neg() } else { beta }, alpha],
        ];
        let reference_norm = if complex {
            norm(&[alpha, beta])
        } else {
            alpha.abs_upper().add(beta)
        };
        let mut defects = [zero; 4];
        for row in 0..2 {
            for column in 0..2 {
                defects[row * 2 + column] = transformed[row][column].sub(reference[row][column]);
            }
        }
        let ideal_decay = reference_norm.add(norm(&defects)).hi;
        let drive = [b1.sub(a1.mul(b0)), b2.sub(a2.mul(b0))];
        let transformed_drive = [
            drive[0],
            inverse[1][0].mul(drive[0]).add(inverse[1][1].mul(drive[1])),
        ];
        let ideal_drive = norm(&transformed_drive).hi;

        // Grouping eight epsilons bounds rounding against absolute update summands.
        // Separate underflow allowances cover every operation in each TDFII update.
        // The output error also enters both feedback products.
        let g = Interval::point(ROUND);
        let c0 = b0.abs_upper();
        let c1 = b1.abs_upper();
        let c2 = b2.abs_upper();
        let d1 = a1.abs_upper();
        let d2 = a2.abs_upper();
        let row_norm = norm(&[alpha, beta]);
        let inverse_norm = norm(&[one, inverse[1][0], inverse[1][1]]);
        let output_input = one.add(g).mul(c0);
        let output_state = one.add(g);
        let error_input = [
            d1.mul(g).mul(c0).add(g.mul(c1.add(d1.mul(output_input)))),
            d2.mul(g).mul(c0).add(g.mul(c2.add(d2.mul(output_input)))),
        ];
        let error_state = [
            d1.mul(g).add(g.mul(d1.mul(output_state).add(row_norm))),
            d2.mul(g).add(g.mul(d2.mul(output_state))),
        ];
        let tiny = Interval::point(2.0 * TINY);
        let update_tiny = Interval::point(8.0 * TINY);
        let error_floor = [
            d1.mul(tiny).add(g.mul(d1).mul(tiny)).add(update_tiny),
            d2.mul(tiny).add(g.mul(d2).mul(tiny)).add(update_tiny),
        ];
        let runtime_decay = Interval::point(ideal_decay)
            .add(inverse_norm.mul(norm(&error_state)))
            .hi;
        let runtime_drive = Interval::point(ideal_drive)
            .add(inverse_norm.mul(norm(&error_input)))
            .hi;
        let runtime_floor = inverse_norm.mul(norm(&error_floor)).hi;
        let coarse_l1 = b0
            .abs_upper()
            .add(Interval::point(ideal_drive).div(one.sub(Interval::point(ideal_decay))))
            .hi;
        let ideal_l1 = real_pole_l1(coefficients).map_or(coarse_l1, |bound| bound.min(coarse_l1));
        let design = Self {
            coefficients,
            channels,
            ideal_decay,
            ideal_drive,
            ideal_l1,
            runtime_decay,
            runtime_drive,
            runtime_floor,
        };
        if !(0.0..1.0).contains(&runtime_decay)
            || ![
                ideal_drive,
                runtime_drive,
                runtime_floor,
                design.full_l1_bound(),
            ]
            .iter()
            .all(|x| x.is_finite())
        {
            return Err(PeqError::ProofBudget);
        }
        Ok(design)
    }

    /// Normalized coefficients in [b0,b1,b2,a1,a2] order.
    pub fn coefficients(self) -> [f64; 5] {
        self.coefficients
    }

    /// Absolute impulse sum of the exact linear recurrence using the chosen f64 coefficients.
    pub fn full_l1_bound(self) -> f64 {
        self.ideal_l1
    }

    /// Includes recurrent runtime rounding and the additive underflow allowance.
    pub fn peak_bound(self, input_peak: f64) -> Result<f64, PeqError> {
        if !input_peak.is_finite() || input_peak < 0.0 {
            return Err(PeqError::Parameters);
        }
        let g = Interval::point(1.0).add(Interval::point(ROUND));
        let coarse = g
            .mul(
                Interval::point(self.coefficients[0].abs())
                    .mul(Interval::point(input_peak))
                    .add(self.state_bound(input_peak)),
            )
            .add(Interval::point(2.0 * TINY))
            .hi;
        // Separate real poles can give a much tighter exact impulse sum than one modal norm.
        // Runtime state errors still use the conservative modal recurrence.
        let state_error_drive = Interval::point(self.runtime_decay)
            .sub(Interval::point(self.ideal_decay))
            .mul(self.state_bound(input_peak))
            .add(
                Interval::point(self.runtime_drive)
                    .sub(Interval::point(self.ideal_drive))
                    .mul(Interval::point(input_peak)),
            )
            .add(Interval::point(self.runtime_floor));
        let state_error =
            state_error_drive.div(Interval::point(1.0).sub(Interval::point(self.ideal_decay)));
        let tight = Interval::point(self.ideal_l1)
            .mul(Interval::point(input_peak))
            .add(state_error)
            .add(
                Interval::point(ROUND).mul(
                    Interval::point(self.coefficients[0].abs())
                        .mul(Interval::point(input_peak))
                        .add(self.state_bound(input_peak)),
                ),
            )
            .add(Interval::point(2.0 * TINY))
            .hi;
        let bound = coarse.min(tight);
        if bound.is_finite() {
            Ok(bound)
        } else {
            Err(PeqError::Numerical)
        }
    }

    fn state_bound(self, input_peak: f64) -> Interval {
        Interval::point(self.runtime_drive)
            .mul(Interval::point(input_peak))
            .add(Interval::point(self.runtime_floor))
            .div(Interval::point(1.0).sub(Interval::point(self.runtime_decay)))
    }

    /// Chooses zero-input frames. This is bounded IIR truncation, never natural support.
    pub fn tail(
        self,
        input_peak: f64,
        downstream_gain: f64,
        error_budget: f64,
    ) -> Result<PeqTail, PeqError> {
        if !input_peak.is_finite()
            || input_peak < 0.0
            || !downstream_gain.is_finite()
            || downstream_gain < 0.0
            || !error_budget.is_finite()
            || error_budget <= 0.0
        {
            return Err(PeqError::Parameters);
        }
        self.peak_bound(input_peak)?;
        let residual = |frames| {
            let decay = power_upper(self.runtime_decay, frames);
            let floor = Interval::point(self.runtime_floor)
                .div(Interval::point(1.0).sub(Interval::point(self.runtime_decay)));
            Interval::point(1.0)
                .add(Interval::point(ROUND))
                .mul(
                    self.state_bound(input_peak)
                        .mul(Interval::point(decay))
                        .add(floor),
                )
                .add(Interval::point(2.0 * TINY))
                .mul(Interval::point(downstream_gain))
                .hi
        };
        if residual(MAX_TAIL_FRAMES) > error_budget {
            return Err(PeqError::TailBudget);
        }
        let mut low = 0;
        let mut high = MAX_TAIL_FRAMES;
        while low < high {
            let middle = low + (high - low) / 2;
            if residual(middle) <= error_budget {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        Ok(PeqTail {
            coefficient_bits: self.coefficients.map(f64::to_bits),
            channels: self.channels,
            input_peak,
            frames: low,
            discarded_output_bound: residual(low),
        })
    }

    pub fn instantiate(self, tail: PeqTail) -> Result<PreparedPeq, PeqError> {
        if tail.coefficient_bits != self.coefficients.map(f64::to_bits)
            || tail.channels != self.channels
        {
            return Err(PeqError::Parameters);
        }
        Ok(PreparedPeq {
            design: self,
            tail,
            state: [[0.0; 2]; 2],
            lifecycle: Lifecycle::Processing,
            remaining: 0,
        })
    }

    pub fn resources(self) -> PeqResources {
        PeqResources {
            resident_bytes: size_of::<PreparedPeq>(),
            // Audited logical scratch allowance; no heap scratch or measured stack claim.
            preparation_scratch_bytes: 8192,
            state_scalars: self.channels.count() * 2,
            filter_operations_per_frame: self.channels.count() * 9,
            proof_matrix_products: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Processing,
    Draining,
    Finished,
    Cancelled,
    Failed,
}

#[derive(Debug)]
pub struct PreparedPeq {
    design: PeqDesign,
    tail: PeqTail,
    state: [[f64; 2]; 2],
    lifecycle: Lifecycle,
    remaining: usize,
}
impl PreparedPeq {
    pub fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<(), PeqError> {
        if self.lifecycle != Lifecycle::Processing {
            return self.fail(output, PeqError::Lifecycle);
        }
        let channels = self.design.channels.count();
        if input.len() != output.len() || !input.len().is_multiple_of(channels) {
            return self.fail(output, PeqError::Span);
        }
        for &sample in input {
            if !sample.is_finite() {
                return self.fail(output, PeqError::NonFinite);
            }
            if sample.abs() > self.tail.input_peak {
                return self.fail(output, PeqError::InputPeak);
            }
        }
        for (index, (&sample, destination)) in input.iter().zip(output.iter_mut()).enumerate() {
            let y = self.sample(sample, index % channels);
            if !y.is_finite() {
                return self.fail(output, PeqError::Numerical);
            }
            *destination = y;
        }
        Ok(())
    }

    pub fn begin_drain(&mut self) -> Result<(), PeqError> {
        if self.lifecycle != Lifecycle::Processing {
            return Err(PeqError::Lifecycle);
        }
        self.remaining = self.tail.frames;
        self.lifecycle = Lifecycle::Draining;
        Ok(())
    }

    /// Returns emitted frames. Output after those frames is zero-filled.
    pub fn drain(&mut self, output: &mut [f64]) -> Result<usize, PeqError> {
        output.fill(0.0);
        if self.lifecycle == Lifecycle::Finished {
            return Ok(0);
        }
        if self.lifecycle != Lifecycle::Draining {
            return self.fail(output, PeqError::Lifecycle);
        }
        let channels = self.design.channels.count();
        if !output.len().is_multiple_of(channels) || (output.is_empty() && self.remaining > 0) {
            return self.fail(output, PeqError::Span);
        }
        let frames = self.remaining.min(output.len() / channels);
        for (index, destination) in output[..frames * channels].iter_mut().enumerate() {
            let y = self.sample(0.0, index % channels);
            if !y.is_finite() {
                return self.fail(output, PeqError::Numerical);
            }
            *destination = y;
        }
        self.remaining -= frames;
        if self.remaining == 0 {
            self.state = [[0.0; 2]; 2];
            self.lifecycle = Lifecycle::Finished;
        }
        Ok(frames)
    }

    pub fn reset(&mut self) {
        self.state = [[0.0; 2]; 2];
        self.remaining = 0;
        self.lifecycle = Lifecycle::Processing;
    }
    pub fn cancel(&mut self) {
        self.reset();
        self.lifecycle = Lifecycle::Cancelled;
    }
    fn fail<T>(&mut self, output: &mut [f64], error: PeqError) -> Result<T, PeqError> {
        output.fill(0.0);
        self.reset();
        self.lifecycle = Lifecycle::Failed;
        Err(error)
    }
    fn sample(&mut self, input: f64, channel: usize) -> f64 {
        let [b0, b1, b2, a1, a2] = self.design.coefficients;
        let state = &mut self.state[channel];
        let output = b0 * input + state[0];
        state[0] = (b1 * input - a1 * output) + state[1];
        state[1] = b2 * input - a2 * output;
        if state.iter().all(|x| x.is_finite()) {
            output
        } else {
            f64::NAN
        }
    }
}

fn real_pole_l1(coefficients: [f64; 5]) -> Option<f64> {
    let [b0, b1, b2, a1, a2] = coefficients.map(Interval::point);
    let one = Interval::point(1.0);
    let alpha = a1.neg().div(Interval::point(2.0));
    let squared = alpha.mul(alpha).sub(a2);
    if squared.lo <= 0.0 {
        return None;
    }
    let beta = Interval {
        lo: squared.lo.sqrt().next_down(),
        hi: squared.hi.sqrt().next_up(),
    };
    let plus = alpha.add(beta);
    let minus = alpha.sub(beta);
    if plus.abs_upper().hi >= 1.0 || minus.abs_upper().hi >= 1.0 {
        return None;
    }
    let h1 = b1.sub(a1.mul(b0));
    let h2 = b2.sub(a1.mul(h1)).sub(a2.mul(b0));
    let gap = plus.sub(minus);
    let residue_plus = h2.sub(minus.mul(h1)).div(gap);
    let residue_minus = plus.mul(h1).sub(h2).div(gap);
    let bound = b0
        .abs_upper()
        .add(residue_plus.abs_upper().div(one.sub(plus.abs_upper())))
        .add(residue_minus.abs_upper().div(one.sub(minus.abs_upper())))
        .hi;
    bound.is_finite().then_some(bound)
}

fn cookbook(filter: &PeqFilter, rate: u32) -> Result<[f64; 5], PeqError> {
    let (frequency, gain) = match *filter {
        PeqFilter::Peaking {
            frequency_hz,
            gain_db,
            ..
        }
        | PeqFilter::LowShelf {
            frequency_hz,
            gain_db,
        }
        | PeqFilter::HighShelf {
            frequency_hz,
            gain_db,
        } => (frequency_hz.get(), gain_db.get()),
    };
    if gain == 0.0 {
        return Ok([1.0, 0.0, 0.0, 0.0, 0.0]);
    }
    // Robert Bristow-Johnson, https://www.w3.org/TR/audio-eq-cookbook/ .
    let a = 10_f64.powf(gain / 40.0);
    let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(rate);
    let (sine, cosine) = omega.sin_cos();
    let (b, denominator) = match *filter {
        PeqFilter::Peaking { q, .. } => {
            let alpha = sine / (2.0 * q.get());
            (
                [1.0 + alpha * a, -2.0 * cosine, 1.0 - alpha * a],
                [1.0 + alpha / a, -2.0 * cosine, 1.0 - alpha / a],
            )
        }
        PeqFilter::LowShelf { .. } => {
            let v = (2.0 * a).sqrt() * sine;
            (
                [
                    a * ((a + 1.0) - (a - 1.0) * cosine + v),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cosine),
                    a * ((a + 1.0) - (a - 1.0) * cosine - v),
                ],
                [
                    (a + 1.0) + (a - 1.0) * cosine + v,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cosine),
                    (a + 1.0) + (a - 1.0) * cosine - v,
                ],
            )
        }
        PeqFilter::HighShelf { .. } => {
            let v = (2.0 * a).sqrt() * sine;
            (
                [
                    a * ((a + 1.0) + (a - 1.0) * cosine + v),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cosine),
                    a * ((a + 1.0) + (a - 1.0) * cosine - v),
                ],
                [
                    (a + 1.0) - (a - 1.0) * cosine + v,
                    2.0 * ((a - 1.0) - (a + 1.0) * cosine),
                    (a + 1.0) - (a - 1.0) * cosine - v,
                ],
            )
        }
    };
    let [a0, a1, a2] = denominator;
    if !a0.is_finite() || a0 <= 0.0 {
        return Err(PeqError::NonFinite);
    }
    let [b0, b1, b2] = b;
    Ok([b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0])
}

#[derive(Debug, Clone, Copy)]
struct Interval {
    lo: f64,
    hi: f64,
}
impl Interval {
    fn point(value: f64) -> Self {
        Self {
            lo: value,
            hi: value,
        }
    }
    fn add(self, other: Self) -> Self {
        Self {
            lo: (self.lo + other.lo).next_down(),
            hi: (self.hi + other.hi).next_up(),
        }
    }
    fn neg(self) -> Self {
        Self {
            lo: -self.hi,
            hi: -self.lo,
        }
    }
    fn sub(self, other: Self) -> Self {
        self.add(other.neg())
    }
    fn mul(self, other: Self) -> Self {
        let products = [
            self.lo * other.lo,
            self.lo * other.hi,
            self.hi * other.lo,
            self.hi * other.hi,
        ];
        Self {
            lo: products
                .into_iter()
                .fold(f64::INFINITY, f64::min)
                .next_down(),
            hi: products
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max)
                .next_up(),
        }
    }
    fn div(self, other: Self) -> Self {
        if other.lo <= 0.0 && other.hi >= 0.0 {
            return Self {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
            };
        }
        self.mul(Self {
            lo: (1.0 / other.hi).next_down(),
            hi: (1.0 / other.lo).next_up(),
        })
    }
    fn abs_upper(self) -> Self {
        Self::point(self.lo.abs().max(self.hi.abs()))
    }
}
fn norm(values: &[Interval]) -> Interval {
    let mut squared = Interval::point(0.0);
    for &value in values {
        let magnitude = value.abs_upper();
        squared = squared.add(magnitude.mul(magnitude));
    }
    Interval {
        lo: 0.0,
        hi: squared.hi.sqrt().next_up(),
    }
}
fn product(a: [[Interval; 2]; 2], b: [[Interval; 2]; 2]) -> [[Interval; 2]; 2] {
    let mut result = [[Interval::point(0.0); 2]; 2];
    for row in 0..2 {
        for column in 0..2 {
            result[row][column] = a[row][0].mul(b[0][column]).add(a[row][1].mul(b[1][column]));
        }
    }
    result
}
fn power_upper(mut base: f64, mut exponent: usize) -> f64 {
    let mut result = 1.0;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = (result * base).next_up();
        }
        exponent >>= 1;
        if exponent > 0 {
            base = (base * base).next_up();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_jury_margin_rejects_unstable_and_uncertain_denominators() {
        for denominator in [
            [-2.0, 1.0],
            [0.0, 1.0],
            [0.0, 1.0 - 0.5e-10],
            [2.0, 0.0],
            [-2.0, 0.0],
        ] {
            assert!(matches!(
                PeqDesign::from_coefficients(
                    [1.0, 0.0, 0.0, denominator[0], denominator[1]],
                    ChannelLayout::Mono
                ),
                Err(PeqError::Stability)
            ));
        }
        assert!(
            PeqDesign::from_coefficients([1.0, 0.0, 0.0, 0.0, 1.0 - 2e-10], ChannelLayout::Mono)
                .is_ok()
        );
    }
    #[test]
    fn coefficient_nonfinites_and_unprovable_bounds_reject_before_state_creation() {
        assert!(matches!(
            PeqDesign::from_coefficients([f64::NAN, 0.0, 0.0, 0.0, 0.0], ChannelLayout::Mono),
            Err(PeqError::NonFinite)
        ));
        assert!(matches!(
            PeqDesign::from_coefficients(
                [f64::MAX, f64::MAX, f64::MAX, 0.0, 0.0],
                ChannelLayout::Mono
            ),
            Err(PeqError::ProofBudget)
        ));
    }
    #[test]
    fn repeated_and_nearly_repeated_poles_have_finite_enclosures() {
        for a2 in [0.81 - 1e-14, 0.81, 0.81 + 1e-14] {
            let design =
                PeqDesign::from_coefficients([1.0, 0.0, 0.0, -1.8, a2], ChannelLayout::Mono)
                    .unwrap();
            let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
            let mut kernel = design.instantiate(tail).unwrap();
            let mut input = [0.0; 4096];
            input[0] = 1.0;
            let mut output = [0.0; 4096];
            kernel.process(&input, &mut output).unwrap();
            assert!(output.iter().map(|x| x.abs()).sum::<f64>() <= design.full_l1_bound());
            assert!(kernel.state.iter().flatten().all(|x| x.is_finite()));
        }
    }
}
