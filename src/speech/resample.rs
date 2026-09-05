//! 2x upsampler for 16-bit mono audio (22050 Hz espeak-ng output → 44100 Hz).
//!
//! WSLg's PulseAudio server resamples everything to its 44100 Hz RDP sink
//! with `speex-float-1`, the lowest-quality setting, and its configuration
//! cannot be changed. Delivering audio at the sink rate sidesteps that
//! resampler entirely (the mono → stereo remix that remains is lossless).
//!
//! The filter is a windowed-sinc half-band FIR: even-phase outputs are the
//! input samples themselves (delayed), odd-phase outputs are interpolated.

/// Number of FIR taps (odd; `(TAPS - 1) / 2` must be even so the centre tap
/// lands on an input sample).
const TAPS: usize = 33;

/// Half-band 2x upsampler with the state needed to stream chunks.
pub struct Upsampler {
    /// Odd-phase (interpolation) coefficients, applied to consecutive input
    /// samples; length `(TAPS + 1) / 2`.
    odd: Vec<f32>,
    /// The most recent input samples, oldest first; length `odd.len()`.
    history: Vec<f32>,
}

impl Default for Upsampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Upsampler {
    pub fn new() -> Self {
        let centre = (TAPS - 1) / 2;
        debug_assert_eq!(centre % 2, 0);
        // Windowed sinc with cutoff at a quarter of the output rate; only the
        // odd taps matter (even taps of a half-band filter are zero except
        // the centre, which is the pass-through path).
        let mut odd: Vec<f32> = (0..TAPS)
            .filter(|i| i % 2 == 1)
            .map(|i| {
                let x = (i as f64 - centre as f64) / 2.0;
                let sinc = (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x);
                let window =
                    0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (TAPS - 1) as f64).cos();
                (sinc * window) as f32
            })
            .collect();
        // Unity gain for the interpolated phase
        let sum: f32 = odd.iter().sum();
        for c in odd.iter_mut() {
            *c /= sum;
        }
        let n = odd.len();
        Self {
            odd,
            history: vec![0.0; n],
        }
    }

    /// Upsample a chunk; the output has exactly twice as many samples. Output
    /// is delayed by `(TAPS - 1) / 4` input samples, which is inaudible.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        let n = self.odd.len();
        let mut out = Vec::with_capacity(input.len() * 2);
        for &sample in input {
            self.history.rotate_left(1);
            self.history[n - 1] = sample as f32;
            // Even phase: the input sample at the filter centre
            out.push(clamp(self.history[n / 2 - 1]));
            // Odd phase: interpolate between the centre sample and the next
            let acc: f32 = self
                .odd
                .iter()
                .zip(self.history.iter().rev())
                .map(|(c, h)| c * h)
                .sum();
            out.push(clamp(acc));
        }
        out
    }

    /// Forget any history (between utterances).
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|h| *h = 0.0);
    }
}

fn clamp(v: f32) -> i16 {
    v.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, n: usize, amp: f64) -> Vec<f64> {
        (0..n)
            .map(|i| amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin())
            .collect()
    }

    #[test]
    fn doubles_the_sample_count() {
        let mut up = Upsampler::new();
        assert_eq!(up.process(&[0; 441]).len(), 882);
        assert_eq!(up.process(&[]).len(), 0);
    }

    #[test]
    fn reproduces_a_sine_at_the_output_rate() {
        // 1 kHz at 22050 Hz in, compare against 1 kHz at 44100 Hz out
        let input: Vec<i16> = sine(1000.0, 22050.0, 2205, 10000.0)
            .iter()
            .map(|v| *v as i16)
            .collect();
        let mut up = Upsampler::new();
        let output = up.process(&input);
        let delay = (TAPS - 1) / 2; // output samples
        let expected = sine(1000.0, 44100.0, output.len(), 10000.0);
        // Skip the filter's warm-up, compare the steady state
        let mut err = 0.0;
        let mut count = 0;
        for (i, &o) in output.iter().enumerate().skip(TAPS * 2) {
            let e = expected[i - delay];
            err += (o as f64 - e).powi(2);
            count += 1;
        }
        let rms = (err / count as f64).sqrt();
        assert!(rms < 60.0, "rms error {} (0.6% of amplitude)", rms);
    }

    #[test]
    fn passes_dc_with_unity_gain() {
        let mut up = Upsampler::new();
        let out = up.process(&[1000; 200]);
        for &s in &out[TAPS * 2..] {
            assert!((s - 1000).abs() <= 1, "dc sample {}", s);
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut up = Upsampler::new();
        assert!(up.process(&[0; 100]).iter().all(|&s| s == 0));
    }
}
