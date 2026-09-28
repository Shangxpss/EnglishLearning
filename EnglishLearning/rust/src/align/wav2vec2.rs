//! wav2vec2-base-960h forward pass, hand-written.
//!
//! Why hand-written: the alternatives all fail on this machine. `tract-onnx`
//! cannot run either shipped ONNX graph (Xenova's int8 uses `ConvInteger` with
//! mixed u8/i8, which tract refuses to unify; the fp16 graph dies on symbolic
//! shape inference, and tract 0.23 has no `concretize_dims` to bind the symbols).
//! `ort` has no `static` feature in 2.0.0-rc.13, so it would ship a shared
//! library next to the binary — and its `download-binaries` needs
//! `cdn.pyke.io`, which no mirror covers here.
//!
//! Hand-writing the encoder keeps the promise made in `Cargo.toml`: no new
//! dependencies, no runtime downloads beyond the model file itself.
//!
//! # Architecture (verified against `modeling_wav2vec2.py`)
//!
//! `config.json` says `feat_extract_norm: "group"` and
//! `do_stable_layer_norm: false`, which picks specific code paths:
//!
//! ```text
//! input (16 kHz, normalised zero-mean unit-variance)
//!   └─ feature extractor, 7 convs, strides 5·2·2·2·2·2·2 = 320 (20 ms frames)
//!        conv0: Conv1d(1→512, k=10, s=5, no bias) → GroupNorm(512 groups) → GELU
//!        conv1..6: Conv1d(k=3/3/3/3/2/2, s=2, no bias) → GELU      (no norm)
//!   └─ feature projection: LayerNorm(512) → Linear(512→768)
//!   └─ positional conv: grouped Conv1d(768, k=128, groups=16) + weight-norm
//!        hidden = hidden + pos_conv(hidden)      ← residual, then:
//!        hidden = LayerNorm(768)
//!   └─ 12 × post-norm transformer layer:
//!        r = h; h = r + attn(h); h = LayerNorm(h)
//!        h = h + ffn(h);         h = LayerNorm(h)
//!   └─ lm_head: Linear(768→32) → CTC emissions (blank = 0)
//! ```
//!
//! Both `feat_extract_norm` and `do_stable_layer_norm` change real behaviour:
//! the "group" branch normalises only after the first conv, and the non-stable
//! branch normalises *after* each residual add rather than before.

use super::safetensors::Safetensors;
use super::tensor;
use std::path::Path;

pub const SAMPLE_RATE: u32 = 16_000;
/// Input samples consumed per output frame (5·2·2·2·2·2·2).
pub const SAMPLES_PER_FRAME: usize = 320;

const HIDDEN: usize = 768;
const HEADS: usize = 12;
const HEAD_DIM: usize = HIDDEN / HEADS;
const N_LAYERS: usize = 12;
const INTERMEDIATE: usize = 3072;
const CONV_DIM: usize = 512;
const POS_KERNEL: usize = 128;
const POS_GROUPS: usize = 16;
pub const VOCAB: usize = 32;
const LAYER_NORM_EPS: f32 = 1e-5;
const GROUP_NORM_EPS: f32 = 1e-5;
/// `head_dim ** -0.5`, i.e. 64 ** -0.5.
const ATTENTION_SCALE: f32 = 0.125;

const CONV_KERNEL: [usize; 7] = [10, 3, 3, 3, 3, 2, 2];
const CONV_STRIDE: [usize; 7] = [5, 2, 2, 2, 2, 2, 2];

/// One transformer layer's parameters (post-norm layout).
struct Layer {
    ln_w: Vec<f32>,
    ln_b: Vec<f32>,
    q_w: Vec<f32>,
    q_b: Vec<f32>,
    k_w: Vec<f32>,
    k_b: Vec<f32>,
    v_w: Vec<f32>,
    v_b: Vec<f32>,
    out_w: Vec<f32>,
    out_b: Vec<f32>,
    ff_in_w: Vec<f32>,
    ff_in_b: Vec<f32>,
    ff_out_w: Vec<f32>,
    ff_out_b: Vec<f32>,
    fln_w: Vec<f32>,
    fln_b: Vec<f32>,
}

/// Conv + GroupNorm + activation of feature-extractor layer 0.
struct ConvLayer0 {
    w: Vec<f32>,
    gn_w: Vec<f32>,
    gn_b: Vec<f32>,
}

/// A loaded wav2vec2-base-960h model. All weights are `f32`, ~378 MB total.
pub struct Wav2Vec2 {
    conv0: ConvLayer0,
    /// Feature-extractor layers 1..7 (conv + GELU, no normalisation).
    conv_rest: Vec<Vec<f32>>,
    proj_ln_w: Vec<f32>,
    proj_ln_b: Vec<f32>,
    proj_w: Vec<f32>,
    proj_b: Vec<f32>,
    /// Positional convolution kernel, already reconstructed from weight-norm.
    pos_w: Vec<f32>,
    pos_b: Vec<f32>,
    enc_ln_w: Vec<f32>,
    enc_ln_b: Vec<f32>,
    layers: Vec<Layer>,
    lm_w: Vec<f32>,
    lm_b: Vec<f32>,
}

impl Wav2Vec2 {
    /// Load all weights from a safetensors file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut st = Safetensors::open(path).map_err(|e| format!("wav2vec2 weights: {e}"))?;

        let mut conv0 = ConvLayer0 {
            w: st.f32("wav2vec2.feature_extractor.conv_layers.0.conv.weight")?,
            gn_w: st.f32("wav2vec2.feature_extractor.conv_layers.0.layer_norm.weight")?,
            gn_b: st.f32("wav2vec2.feature_extractor.conv_layers.0.layer_norm.bias")?,
        };
        // Guard the shape assumptions the kernels rely on.
        if conv0.w.len() != CONV_DIM * CONV_KERNEL[0] {
            return Err(format!(
                "conv0 weight has {} elements, expected {}",
                conv0.w.len(),
                CONV_DIM * CONV_KERNEL[0]
            ));
        }
        if conv0.gn_w.len() != CONV_DIM || conv0.gn_b.len() != CONV_DIM {
            return Err("conv0 group-norm affine weights have unexpected size".to_string());
        }

        let mut conv_rest = Vec::with_capacity(6);
        for i in 1..7 {
            conv_rest.push(st.f32(&format!(
                "wav2vec2.feature_extractor.conv_layers.{i}.conv.weight"
            ))?);
        }
        trace("w-conv0", &conv0.w);
        trace("w-conv1", &conv_rest[0]);

        // Positional convolution: stored as weight-norm (g, v), needs rebuilding.
        let pos_g = st.f32("wav2vec2.encoder.pos_conv_embed.conv.weight_g")?;
        let pos_v = st.f32("wav2vec2.encoder.pos_conv_embed.conv.weight_v")?;
        let in_per_group = HIDDEN / POS_GROUPS;
        if pos_g.len() != POS_KERNEL || pos_v.len() != HIDDEN * in_per_group * POS_KERNEL {
            return Err(format!(
                "pos-conv weight-norm tensors have unexpected sizes (g={}, v={})",
                pos_g.len(),
                pos_v.len()
            ));
        }
        let pos_w =
            tensor::reconstruct_weight_norm(&pos_v, &pos_g, HIDDEN, in_per_group, POS_KERNEL);

        let mut layers = Vec::with_capacity(N_LAYERS);
        for i in 0..N_LAYERS {
            let p = format!("wav2vec2.encoder.layers.{i}");
            layers.push(Layer {
                ln_w: st.f32(&format!("{p}.layer_norm.weight"))?,
                ln_b: st.f32(&format!("{p}.layer_norm.bias"))?,
                q_w: st.f32(&format!("{p}.attention.q_proj.weight"))?,
                q_b: st.f32(&format!("{p}.attention.q_proj.bias"))?,
                k_w: st.f32(&format!("{p}.attention.k_proj.weight"))?,
                k_b: st.f32(&format!("{p}.attention.k_proj.bias"))?,
                v_w: st.f32(&format!("{p}.attention.v_proj.weight"))?,
                v_b: st.f32(&format!("{p}.attention.v_proj.bias"))?,
                out_w: st.f32(&format!("{p}.attention.out_proj.weight"))?,
                out_b: st.f32(&format!("{p}.attention.out_proj.bias"))?,
                ff_in_w: st.f32(&format!("{p}.feed_forward.intermediate_dense.weight"))?,
                ff_in_b: st.f32(&format!("{p}.feed_forward.intermediate_dense.bias"))?,
                ff_out_w: st.f32(&format!("{p}.feed_forward.output_dense.weight"))?,
                ff_out_b: st.f32(&format!("{p}.feed_forward.output_dense.bias"))?,
                fln_w: st.f32(&format!("{p}.final_layer_norm.weight"))?,
                fln_b: st.f32(&format!("{p}.final_layer_norm.bias"))?,
            });
        }

        Ok(Self {
            conv0: ConvLayer0 {
                w: std::mem::take(&mut conv0.w),
                gn_w: std::mem::take(&mut conv0.gn_w),
                gn_b: std::mem::take(&mut conv0.gn_b),
            },
            conv_rest,
            proj_ln_w: st.f32("wav2vec2.feature_projection.layer_norm.weight")?,
            proj_ln_b: st.f32("wav2vec2.feature_projection.layer_norm.bias")?,
            proj_w: st.f32("wav2vec2.feature_projection.projection.weight")?,
            proj_b: st.f32("wav2vec2.feature_projection.projection.bias")?,
            pos_w,
            pos_b: st.f32("wav2vec2.encoder.pos_conv_embed.conv.bias")?,
            enc_ln_w: st.f32("wav2vec2.encoder.layer_norm.weight")?,
            enc_ln_b: st.f32("wav2vec2.encoder.layer_norm.bias")?,
            layers,
            lm_w: st.f32("lm_head.weight")?,
            lm_b: st.f32("lm_head.bias")?,
        })
    }

    /// Exact number of output frames for `n` input samples.
    ///
    /// Applies PyTorch's `floor((L - k)/stride) + 1` once per conv layer rather
    /// than assuming `n / 320`, which is off by a frame or two at the edges and
    /// would shift every timestamp.
    pub fn frame_count(n: usize) -> usize {
        let mut len = n;
        for i in 0..7 {
            let (k, s) = (CONV_KERNEL[i], CONV_STRIDE[i]);
            if len < k {
                return 0;
            }
            len = (len - k) / s + 1;
        }
        len
    }

    /// CTC emissions for 16 kHz mono audio: `(logits, frames, vocab)`.
    ///
    /// Row `f` holds the scores for output frame `f` (20 ms). Input is
    /// normalised per utterance exactly as `Wav2Vec2FeatureExtractor` does.
    pub fn emissions(&self, samples: &[f32]) -> Result<(Vec<f32>, usize), String> {
        let t_frames = Self::frame_count(samples.len());
        if t_frames == 0 {
            return Err(format!(
                "audio too short for wav2vec2: {} samples yields no frames (needs ≥ {} samples, 20 ms)",
                samples.len(),
                SAMPLES_PER_FRAME
            ));
        }

        // 1. Zero-mean unit-variance normalisation (population variance).
        let mut x: Vec<f32> = samples.to_vec();
        let n = x.len() as f32;
        let mean = x.iter().sum::<f32>() / n;
        let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        let inv_std = 1.0 / (var + 1e-7).sqrt();
        for v in x.iter_mut() {
            *v = (*v - mean) * inv_std;
        }

        // 2. Feature extractor, held channel-major ([C][T]) as Conv1d expects.
        let (mut feat, mut t) = tensor::conv1d(
            &x,
            1,
            x.len(),
            &self.conv0.w,
            CONV_DIM,
            CONV_KERNEL[0],
            CONV_STRIDE[0],
            1,
            0,
            None,
            false,
        );
        trace("input", &x);
        trace("conv0", &feat);
        if t == 0 {
            return Err("audio too short for the first convolution".to_string());
        }
        tensor::group_norm_per_channel(
            &mut feat,
            CONV_DIM,
            t,
            &self.conv0.gn_w,
            &self.conv0.gn_b,
            GROUP_NORM_EPS,
        );
        tensor::gelu(&mut feat);
        trace("gn0+gelu", &feat);

        for (i, w) in self.conv_rest.iter().enumerate() {
            let layer = i + 1;
            let (next, t_next) = tensor::conv1d(
                &feat,
                CONV_DIM,
                t,
                w,
                CONV_DIM,
                CONV_KERNEL[layer],
                CONV_STRIDE[layer],
                1,
                0,
                None,
                false,
            );
            // Every feature-extractor layer ends in the activation
            // (`Wav2Vec2NoLayerNormConvLayer.forward`: conv → activation), not
            // just layer 0. Without it the stack is linear, and because the
            // trained kernels correlate with their inputs the terms add
            // coherently instead of cancelling: the output ran away by ~4 orders
            // of magnitude and every timestamp downstream was garbage.
            feat = next;
            tensor::gelu(&mut feat);
            t = t_next;
            trace(&format!("feat-conv{layer}"), &feat);
        }
        if t != t_frames {
            return Err(format!(
                "internal error: computed {t_frames} frames but the extractor produced {t}"
            ));
        }

        // 3. Channel-major → row-major ([T][C]) for the Linear/LayerNorm stack.
        let mut h = to_row_major(&feat, CONV_DIM, t);
        trace("feat-extractor", &h);

        // 4. Feature projection: LayerNorm(512) → Linear(512→768).
        tensor::layer_norm(
            &mut h,
            t,
            CONV_DIM,
            &self.proj_ln_w,
            &self.proj_ln_b,
            LAYER_NORM_EPS,
        );
        h = tensor::linear(&h, t, CONV_DIM, &self.proj_w, &self.proj_b, HIDDEN);
        trace("feature-projection", &h);

        // 5. Positional convolution, added residually, then normalised.
        let pos = {
            let ch_major = to_channel_major(&h, HIDDEN, t);
            let (mut c, _) = tensor::conv1d(
                &ch_major,
                HIDDEN,
                t,
                &self.pos_w,
                HIDDEN,
                POS_KERNEL,
                1,
                POS_GROUPS,
                POS_KERNEL / 2,
                Some(&self.pos_b),
                true,
            );
            tensor::gelu(&mut c);
            to_row_major(&c, HIDDEN, t)
        };
        trace("pos-conv", &pos);
        for (v, p) in h.iter_mut().zip(pos.iter()) {
            *v += p;
        }
        tensor::layer_norm(
            &mut h,
            t,
            HIDDEN,
            &self.enc_ln_w,
            &self.enc_ln_b,
            LAYER_NORM_EPS,
        );

        trace("encoder-ln", &h);

        // 6. Twelve post-norm transformer layers.
        for (i, layer) in self.layers.iter().enumerate() {
            h = self.transformer_layer(h, t, layer)?;
            if i < 2 || i + 1 == N_LAYERS {
                trace(&format!("layer-{i}"), &h);
            }
        }

        // 7. CTC head.
        let logits = tensor::linear(&h, t, HIDDEN, &self.lm_w, &self.lm_b, VOCAB);
        trace("logits", &logits);
        Ok((logits, t))
    }

    /// One post-norm encoder layer: `h = LN(h + attn(h))`, then `h = LN(h + ffn(h))`.
    fn transformer_layer(&self, h: Vec<f32>, t: usize, l: &Layer) -> Result<Vec<f32>, String> {
        let residual = h.clone();

        let q = tensor::linear(&h, t, HIDDEN, &l.q_w, &l.q_b, HIDDEN);
        let k = tensor::linear(&h, t, HIDDEN, &l.k_w, &l.k_b, HIDDEN);
        let v = tensor::linear(&h, t, HIDDEN, &l.v_w, &l.v_b, HIDDEN);
        let attn = attention(&q, &k, &v, t);
        let attn = tensor::linear(&attn, t, HIDDEN, &l.out_w, &l.out_b, HIDDEN);

        let mut merged: Vec<f32> = residual
            .iter()
            .zip(attn.iter())
            .map(|(r, a)| r + a)
            .collect();
        tensor::layer_norm(&mut merged, t, HIDDEN, &l.ln_w, &l.ln_b, LAYER_NORM_EPS);

        let mut ff = tensor::linear(&merged, t, HIDDEN, &l.ff_in_w, &l.ff_in_b, INTERMEDIATE);
        tensor::gelu(&mut ff);
        let ff = tensor::linear(&ff, t, INTERMEDIATE, &l.ff_out_w, &l.ff_out_b, HIDDEN);

        let mut out: Vec<f32> = merged.iter().zip(ff.iter()).map(|(m, f)| m + f).collect();
        tensor::layer_norm(&mut out, t, HIDDEN, &l.fln_w, &l.fln_b, LAYER_NORM_EPS);
        Ok(out)
    }
}

/// Stage statistics for `SENTENCE_VIDEO_ALIGN_TRACE=1`.
///
/// A hand-written forward pass can be wrong in ways that are invisible from the
/// final emissions (an all-blank CTC output says nothing about *which* stage
/// broke), so each stage can report its shape and range. Kept because it is the
/// only practical way to compare against a reference implementation.
fn trace(name: &str, v: &[f32]) {
    // Raw dumps let a reference implementation (numpy) compare stage by stage
    // on byte-identical input, which is the only way to see *where* a
    // hand-written forward pass diverges.
    if let Some(dir) = std::env::var_os("SENTENCE_VIDEO_ALIGN_DUMP") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        let _ = std::fs::write(dir.join(format!("{name}.f32")), bytes);
    }

    if std::env::var_os("SENTENCE_VIDEO_ALIGN_TRACE").is_none() {
        return;
    }
    let nonfinite = v.iter().filter(|x| !x.is_finite()).count();
    let (min, max) = v
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| {
            (lo.min(x), hi.max(x))
        });
    let mean = if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    };
    eprintln!(
        "align trace {name}: len={} nonfinite={nonfinite} min={min:.4} max={max:.4} mean={mean:.4}",
        v.len()
    );
}

/// Multi-head self-attention over `[T][768]` inputs, no mask, no dropout.
///
/// Heads are processed one at a time so the score matrix stays `T²` rather than
/// `12·T²` — at 3000 frames that is the difference between 36 MB and 432 MB.
/// Both `T`-indexed loops are split across threads by row (`rows_split_mut`);
/// each row is independent and each element's sum still runs over `j` in the
/// same order, so the result is bit-identical to the sequential version.
fn attention(q: &[f32], k: &[f32], v: &[f32], t: usize) -> Vec<f32> {
    let mut out = vec![0f32; t * HIDDEN];
    let mut scores = vec![0f32; t * t];

    for head in 0..HEADS {
        let off = head * HEAD_DIM;
        // scores[i][j] = (q_i · k_j) * scale
        tensor::rows_split_mut(&mut scores, t, |run, lo| {
            for (r, row) in run.chunks_mut(t).enumerate() {
                let i = lo + r;
                let qi = &q[i * HIDDEN + off..i * HIDDEN + off + HEAD_DIM];
                for (j, s) in row.iter_mut().enumerate() {
                    let kj = &k[j * HIDDEN + off..j * HIDDEN + off + HEAD_DIM];
                    let mut acc = 0f32;
                    for d in 0..HEAD_DIM {
                        acc += qi[d] * kj[d];
                    }
                    *s = acc * ATTENTION_SCALE;
                }
            }
        });
        tensor::softmax(&mut scores, t, t);

        // out[i][off..off+HEAD_DIM] = sum_j scores[i][j] * v_j
        tensor::rows_split_mut(&mut out, HIDDEN, |run, lo| {
            for (r, row) in run.chunks_mut(HIDDEN).enumerate() {
                let i = lo + r;
                let src = &scores[i * t..(i + 1) * t];
                let dst = &mut row[off..off + HEAD_DIM];
                for (j, &w) in src.iter().enumerate() {
                    if w == 0.0 {
                        continue;
                    }
                    let vj = &v[j * HIDDEN + off..j * HIDDEN + off + HEAD_DIM];
                    for d in 0..HEAD_DIM {
                        dst[d] += w * vj[d];
                    }
                }
            }
        });
    }
    out
}

/// `[C][T]` → `[T][C]`.
fn to_row_major(x: &[f32], channels: usize, t: usize) -> Vec<f32> {
    let mut out = vec![0f32; x.len()];
    for c in 0..channels {
        for i in 0..t {
            out[i * channels + c] = x[c * t + i];
        }
    }
    out
}

/// `[T][C]` → `[C][T]`.
fn to_channel_major(x: &[f32], channels: usize, t: usize) -> Vec<f32> {
    let mut out = vec![0f32; x.len()];
    for i in 0..t {
        for c in 0..channels {
            out[c * t + i] = x[i * channels + c];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::tensor;

    #[test]
    fn frame_count_matches_reference_formula() {
        // 16000 samples (1 s) → 49 frames is the well-known wav2vec2 figure.
        assert_eq!(Wav2Vec2::frame_count(16_000), 49);
        // The naive n/320 guess would give 50, which is why the exact chain matters.
        assert_ne!(16_000 / SAMPLES_PER_FRAME, Wav2Vec2::frame_count(16_000));
        assert_eq!(Wav2Vec2::frame_count(319), 0);
    }

    #[test]
    fn transposes_are_inverses() {
        let x: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let cm = to_channel_major(&x, 3, 4);
        assert_eq!(to_row_major(&cm, 3, 4), x);
    }

    /// Attention is where the row split is most aggressive — both `T`-long loops
    /// and the softmax inside it — so it is the one that must be provably
    /// unchanged: the emissions are compared against reference dumps, and a
    /// reassociated sum would show up as a different alignment, not a rounding
    /// artefact. A budget of 1 is the old sequential code.
    #[test]
    fn split_attention_is_bit_identical_to_the_sequential_one() {
        let t = 96;
        let synth = |seed: usize, len: usize| -> Vec<f32> {
            (0..len)
                .map(|i| (((i * 7 + seed) % 29) as f32 - 14.0) / 5.0)
                .collect()
        };
        let q = synth(1, t * HIDDEN);
        let k = synth(2, t * HIDDEN);
        let v = synth(3, t * HIDDEN);

        tensor::set_thread_budget(1);
        let serial = attention(&q, &k, &v, t);

        tensor::set_thread_budget(64);
        let parallel = attention(&q, &k, &v, t);

        assert_eq!(serial, parallel, "attention changed with the row split");
    }

    #[test]
    fn attention_rows_are_convex_combinations_of_values() {
        let t = 3;
        let q = vec![1.0f32; t * HIDDEN];
        let k = vec![1.0f32; t * HIDDEN];
        // v rows are one-hot in the first coordinate, so the output is an average.
        let mut v = vec![0f32; t * HIDDEN];
        for i in 0..t {
            v[i * HIDDEN] = (i + 1) as f32;
        }
        let out = attention(&q, &k, &v, t);
        for i in 0..t {
            let val = out[i * HIDDEN];
            assert!(
                (val - 2.0).abs() < 1e-5,
                "row {i} got {val}, want the mean 2"
            );
        }
        let _ = tensor::gelu;
    }
}
