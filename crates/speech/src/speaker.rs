//! Speaker embedding from MFCC statistics — pure Rust, no model, no downloads.
//!
//! Captures the speaker's vocal timbre (spectral shape over the utterance) far
//! better than the old energy fingerprint: per-frame MFCCs, then the mean and
//! std of each coefficient across the clip → one fixed vector, L2-normalized.
//! Coefficient 0 (log-energy) is dropped so loudness doesn't dominate.
//!
//! A neural ECAPA/wespeaker ONNX model is a future accuracy upgrade behind the
//! same `embed` trait method; this baseline needs nothing extra to run.

use rustfft::{num_complex::Complex, FftPlanner};

use crate::Audio;

const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LEN: usize = 400; // 25 ms @ 16 kHz
const FRAME_SHIFT: usize = 160; // 10 ms
const FFT_SIZE: usize = 512;
const N_MELS: usize = 26;
const N_MFCC: usize = 20; // DCT coefficients computed; we keep c1..c19
const PREEMPH: f32 = 0.97;
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 8000.0;
const EPS: f32 = 1e-10;

/// Number of kept MFCC coefficients (c1..c19).
const KEPT: usize = N_MFCC - 1;
/// Embedding dimensionality: mean + std of each kept coefficient.
pub(crate) const SPEAKER_DIM: usize = KEPT * 2;

/// Compute the speaker embedding. Returns an empty vec if the audio is too short
/// to yield a single frame.
pub(crate) fn speaker_embedding(audio: &Audio) -> Vec<f32> {
    let signal = to_f32_16k(audio);
    if signal.len() < FRAME_LEN {
        return Vec::new();
    }

    // Pre-emphasis.
    let mut pre = vec![0.0f32; signal.len()];
    pre[0] = signal[0];
    for i in 1..signal.len() {
        pre[i] = signal[i] - PREEMPH * signal[i - 1];
    }

    let window = hamming(FRAME_LEN);
    let filterbank = mel_filterbank();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut buf = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];

    // Online mean/variance (Welford) over kept MFCC coefficients.
    let mut count = 0u32;
    let mut mean = [0.0f32; KEPT];
    let mut m2 = [0.0f32; KEPT];

    let mut start = 0;
    while start + FRAME_LEN <= pre.len() {
        // Windowed frame → FFT buffer (zero-padded to FFT_SIZE).
        for (i, b) in buf.iter_mut().enumerate() {
            let v = if i < FRAME_LEN {
                pre[start + i] * window[i]
            } else {
                0.0
            };
            *b = Complex::new(v, 0.0);
        }
        fft.process(&mut buf);

        // Power spectrum (0..=FFT_SIZE/2) → mel energies → log.
        let mut log_mel = [0.0f32; N_MELS];
        for (m, filt) in filterbank.iter().enumerate() {
            let mut energy = 0.0f32;
            for (bin, &w) in filt.iter().enumerate() {
                if w != 0.0 {
                    let c = buf[bin];
                    energy += (c.re * c.re + c.im * c.im) * w;
                }
            }
            log_mel[m] = (energy + EPS).ln();
        }

        // DCT-II → MFCCs, keep c1..c19.
        for (k, slot) in mean.iter_mut().enumerate() {
            let coeff = dct_coeff(&log_mel, k + 1);
            // Welford update.
            count_update(slot, &mut m2[k], coeff, count + 1);
        }
        count += 1;
        start += FRAME_SHIFT;
    }

    if count == 0 {
        return Vec::new();
    }

    // Assemble [mean.., std..] and L2-normalize.
    let mut emb = Vec::with_capacity(SPEAKER_DIM);
    emb.extend_from_slice(&mean);
    for &m2k in &m2 {
        let var = if count > 1 { m2k / (count as f32) } else { 0.0 };
        emb.push(var.sqrt());
    }
    l2_normalize(&mut emb);
    emb
}

/// Welford online update of one coefficient's running mean and M2.
fn count_update(mean: &mut f32, m2: &mut f32, x: f32, n: u32) {
    let delta = x - *mean;
    *mean += delta / n as f32;
    let delta2 = x - *mean;
    *m2 += delta * delta2;
}

fn to_f32_16k(audio: &Audio) -> Vec<f32> {
    let f: Vec<f32> = audio.pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    if audio.sample_rate == 16_000 {
        f
    } else {
        resample_linear(&f, audio.sample_rate, 16_000)
    }
}

fn resample_linear(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let idx = i as f64 * ratio;
        let i0 = idx.floor() as usize;
        let frac = (idx - i0 as f64) as f32;
        let a = input[i0];
        let b = *input.get(i0 + 1).unwrap_or(&a);
        out.push(a * (1.0 - frac) + b * frac);
    }
    out
}

fn hamming(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos())
        .collect()
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Triangular mel filterbank over the power-spectrum bins (0..=FFT_SIZE/2).
fn mel_filterbank() -> Vec<Vec<f32>> {
    let n_bins = FFT_SIZE / 2 + 1;
    let mel_min = hz_to_mel(F_MIN);
    let mel_max = hz_to_mel(F_MAX);
    // N_MELS+2 points → N_MELS triangles.
    let points: Vec<usize> = (0..N_MELS + 2)
        .map(|i| {
            let mel = mel_min + (mel_max - mel_min) * i as f32 / (N_MELS as f32 + 1.0);
            let hz = mel_to_hz(mel);
            ((FFT_SIZE as f32 + 1.0) * hz / SAMPLE_RATE).floor() as usize
        })
        .collect();

    let mut fb = vec![vec![0.0f32; n_bins]; N_MELS];
    for m in 0..N_MELS {
        let (left, center, right) = (points[m], points[m + 1], points[m + 2]);
        for (bin, slot) in fb[m].iter_mut().enumerate().take(n_bins) {
            if bin >= left && bin < center && center > left {
                *slot = (bin - left) as f32 / (center - left) as f32;
            } else if bin >= center && bin < right && right > center {
                *slot = (right - bin) as f32 / (right - center) as f32;
            }
        }
    }
    fb
}

/// A single DCT-II coefficient `k` of `x` (length N_MELS).
fn dct_coeff(x: &[f32; N_MELS], k: usize) -> f32 {
    let m = x.len() as f32;
    let mut sum = 0.0f32;
    for (i, &v) in x.iter().enumerate() {
        sum += v * (std::f32::consts::PI * k as f32 * (i as f32 + 0.5) / m).cos();
    }
    sum
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosine;

    /// A crude vowel-like signal: a fundamental plus two formants.
    fn voice(len: usize, f0: f32, f1: f32, f2: f32) -> Audio {
        let sr = 16_000.0f32;
        let pcm: Vec<i16> = (0..len)
            .map(|i| {
                let t = i as f32 / sr;
                let s = 0.6 * (2.0 * std::f32::consts::PI * f0 * t).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * f1 * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * f2 * t).sin();
                (s * 8000.0) as i16
            })
            .collect();
        Audio::new(pcm, 16_000)
    }

    #[test]
    fn deterministic_and_right_dim() {
        let a = voice(16_000, 120.0, 700.0, 1200.0);
        let e1 = speaker_embedding(&a);
        let e2 = speaker_embedding(&a);
        assert_eq!(e1.len(), SPEAKER_DIM);
        assert!((cosine(&e1, &e2) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn different_voices_are_distinguishable() {
        // Same "speaker" saying it twice vs a different formant profile.
        let me = voice(16_000, 120.0, 700.0, 1200.0);
        let me_again = voice(16_000, 122.0, 690.0, 1210.0);
        let other = voice(16_000, 210.0, 1000.0, 2400.0);
        let self_sim = cosine(&speaker_embedding(&me), &speaker_embedding(&me_again));
        let other_sim = cosine(&speaker_embedding(&me), &speaker_embedding(&other));
        assert!(
            self_sim > other_sim,
            "self {self_sim} should exceed other {other_sim}"
        );
    }

    #[test]
    fn too_short_is_empty() {
        assert!(speaker_embedding(&Audio::new(vec![1, 2, 3], 16_000)).is_empty());
    }
}
