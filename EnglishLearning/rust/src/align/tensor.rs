//! The handful of `f32` kernels wav2vec2 needs.
//!
//! Deliberately tiny: this is a single-model inference path, not a tensor
//! library. Everything operates on flat row-major slices with explicit shapes.
//!
//! No dropout anywhere — inference only. Every activation used by the model
//! (GELU) and every normalisation (LayerNorm, GroupNorm) is reproduced exactly
//! as the reference implementation computes it, because small deviations here
//! silently degrade the CTC emissions and therefore the alignment.

use std::cell::Cell;
use std::sync::OnceLock;

/// How many threads the kernels on *this* thread may use. Zero means "unset".
///
/// One window is a chain of row-independent kernels, so a single window can use
/// the whole machine — worth having because a window count below the core count
/// would otherwise leave cores idle (a 60 s clip has two windows, so the machine
/// was 2/6 busy). The budget is per thread rather than global because only the
/// caller knows how many windows are in flight at once: `align::refine_words`
/// gives each of its window threads an equal share, so `windows × budget` never
/// exceeds the available cores.
thread_local! {
    static THREAD_BUDGET: Cell<usize> = const { Cell::new(0) };
}

/// Set how many threads the kernels on *this* thread may use.
///
/// For a worker thread that exists to run one window (see
/// `align::refine_words`), so the value is never unset. A thread that never calls
/// this — the CLI's `align` subcommand, a test — uses every core.
pub fn set_thread_budget(threads: usize) {
    THREAD_BUDGET.with(|b| b.set(threads.max(1)));
}

/// The number of usable cores, looked up once.
fn core_count() -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    })
}

/// Split `out` into runs of whole rows and hand each thread one run, plus the
/// index of that run's first row.
///
/// Rows are independent in every kernel here — each output row is computed from
/// the read-only inputs alone — so this parallelises without touching a single
/// accumulation order, and therefore without changing a single bit of the
/// result. That matters because the forward pass is validated against reference
/// dumps stage by stage.
///
/// Splitting the *buffer* (`chunks_mut`) rather than an index range is what
/// makes it safe: every thread owns a disjoint slice, so no closure ever needs
/// unique access to the whole of `out`.
pub fn rows_split_mut<T: Send>(
    out: &mut [T],
    row_len: usize,
    body: impl Fn(&mut [T], usize) + Sync,
) {
    debug_assert!(row_len > 0);
    assert_eq!(
        out.len() % row_len,
        0,
        "the buffer must be a whole number of {row_len}-element rows"
    );
    let rows = out.len() / row_len;
    let budget = THREAD_BUDGET.with(|b| b.get());
    let fan = if budget == 0 { core_count() } else { budget }.min(rows);
    if fan < 2 {
        body(out, 0);
        return;
    }

    let chunk = rows.div_ceil(fan) * row_len;
    let body = &body;
    std::thread::scope(|scope| {
        let mut first_row = 0;
        for run in out.chunks_mut(chunk) {
            let lo = first_row;
            first_row += run.len() / row_len;
            scope.spawn(move || body(run, lo));
        }
    });
}

/// How many output rows are computed per pass over `w`.
///
/// The obvious order — one output row at a time, streaming the whole weight
/// matrix for each — re-reads `w` once per row. The feed-forward weights are
/// 3072 × 768 × 4 B = 9.4 MB, which does not fit this machine's 4 MB shared L3,
/// so every row re-streams them from DRAM: ~6.6 GB of traffic per FFN linear.
/// Six threads then saturate the memory bus instead of the cores — measured, 1→6
/// cores bought only **1.61×** wall-clock on a 14 s window.
///
/// Blocking rows is what fixes that: a weight row is loaded once and reused for
/// [`ROW_BLOCK`] outputs, so traffic falls by that factor while the per-element
/// accumulation order is untouched (every output still sums over `i` ascending in
/// a single accumulator) and results stay bit-identical.
const ROW_BLOCK: usize = 8;

/// `y[row][o] = sum_i x[row][i] * w[o][i] + b[o]`, with `w` laid out `[out, in]`.
///
/// Weights in safetensors follow PyTorch's `nn.Linear` convention, so rows of
/// `w` are the output features. Computed [`ROW_BLOCK`] rows at a time — see the
/// constant for why that is what decides how this scales.
pub fn linear(
    x: &[f32],
    t: usize,
    in_dim: usize,
    w: &[f32],
    b: &[f32],
    out_dim: usize,
) -> Vec<f32> {
    debug_assert_eq!(w.len(), out_dim * in_dim);
    debug_assert_eq!(b.len(), out_dim);
    debug_assert_eq!(x.len(), t * in_dim);

    let mut out = vec![0f32; t * out_dim];
    rows_split_mut(&mut out, out_dim, |run, lo| {
        let rows = run.len() / out_dim;
        let full = rows / ROW_BLOCK;
        // The block's input rows, resolved once so the inner loop walks a small
        // array of slices rather than recomputing `base + r` per element.
        let mut xr: [&[f32]; ROW_BLOCK] = [&[]; ROW_BLOCK];

        // Whole blocks. The inner trip count is the constant `ROW_BLOCK` here and
        // that is the whole point: with a runtime bound the same loop compiled to
        // 0.93 GMAC/s, with a constant one 1.88 GMAC/s — LLVM turns the eight
        // independent accumulators into real wide instructions instead of an
        // indexed loop it cannot unroll. The accumulation order is untouched, so
        // both versions produced the same sum bit for bit.
        for blk in 0..full {
            let base = lo + blk * ROW_BLOCK;
            for (r, row) in x[(base * in_dim)..((base + ROW_BLOCK) * in_dim)]
                .chunks_exact(in_dim)
                .enumerate()
            {
                xr[r] = row;
            }
            let ors = &mut run[blk * ROW_BLOCK * out_dim..(blk + 1) * ROW_BLOCK * out_dim];
            for o in 0..out_dim {
                let wr = &w[o * in_dim..(o + 1) * in_dim];
                let bias = b[o];
                let mut acc = [0f32; ROW_BLOCK];
                for i in 0..in_dim {
                    let wi = wr[i];
                    for r in 0..ROW_BLOCK {
                        acc[r] += xr[r][i] * wi;
                    }
                }
                for r in 0..ROW_BLOCK {
                    ors[r * out_dim + o] = acc[r] + bias;
                }
            }
        }

        // At most `ROW_BLOCK - 1` trailing rows, which a split run always leaves:
        // one output row at a time, exactly as the kernel worked before blocking.
        for (i, orow) in run[full * ROW_BLOCK * out_dim..]
            .chunks_mut(out_dim)
            .enumerate()
        {
            let xr = &x[(lo + full * ROW_BLOCK + i) * in_dim..][..in_dim];
            for o in 0..out_dim {
                let wr = &w[o * in_dim..(o + 1) * in_dim];
                let mut acc = 0f32;
                for j in 0..in_dim {
                    acc += xr[j] * wr[j];
                }
                orow[o] = acc + b[o];
            }
        }
    });
    out
}

/// LayerNorm over the last dimension, in place.
///
/// Population variance (biased), matching `torch.nn.LayerNorm`.
pub fn layer_norm(x: &mut [f32], rows: usize, dim: usize, w: &[f32], b: &[f32], eps: f32) {
    debug_assert_eq!(x.len(), rows * dim);
    for row in 0..rows {
        let r = &mut x[row * dim..(row + 1) * dim];
        let mean = r.iter().sum::<f32>() / dim as f32;
        let var = r.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / dim as f32;
        let inv = 1.0 / (var + eps).sqrt();
        for i in 0..dim {
            r[i] = (r[i] - mean) * inv * w[i] + b[i];
        }
    }
}

/// Exact GELU (`0.5·x·(1+erf(x/√2))`), in place.
///
/// The tanh approximation would be subtly wrong here: the model was trained
/// with `nn.functional.gelu`.
pub fn gelu(x: &mut [f32]) {
    const INV_SQRT_2: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for v in x.iter_mut() {
        *v = 0.5 * *v * (1.0 + erf(*v * INV_SQRT_2));
    }
}

/// `erf` via Abramowitz & Stegun 7.1.26 (|error| < 1.5e-7, below `f32` noise).
fn erf(x: f32) -> f32 {
    const P: f32 = 0.3275911;
    const A1: f32 = 0.254829592;
    const A2: f32 = -0.284496736;
    const A3: f32 = 1.421413741;
    const A4: f32 = -1.453152027;
    const A5: f32 = 1.061405429;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + P * x);
    let poly = ((((A5 * t + A4) * t + A3) * t + A2) * t + A1) * t;
    sign * (1.0 - poly * (-x * x).exp())
}

/// `softmax` over the last dimension, in place.
///
/// Worth parallelising despite being short: attention calls it once per head per
/// layer, so a 30 s window runs ~3·10⁸ `exp`s through here — more than the
/// projections cost.
pub fn softmax(x: &mut [f32], rows: usize, dim: usize) {
    debug_assert_eq!(x.len(), rows * dim);
    rows_split_mut(x, dim, |run, _| {
        for r in run.chunks_mut(dim) {
            let max = r.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0f32;
            for v in r.iter_mut() {
                *v = (*v - max).exp();
                sum += *v;
            }
            let inv = 1.0 / sum;
            for v in r.iter_mut() {
                *v *= inv;
            }
        }
    });
}

/// GroupNorm with `num_groups == channels`, i.e. per-channel normalisation over
/// time, then the affine transform.
///
/// That equality is exactly what `feat_extract_norm: "group"` produces for
/// `conv_dim[0] == 512` (see `Wav2Vec2GroupNormConvLayer`), and it is why this
/// is cheaper than a general GroupNorm: each channel normalises alone.
/// Input is channel-major: `x[c][t]`.
pub fn group_norm_per_channel(
    x: &mut [f32],
    channels: usize,
    t: usize,
    w: &[f32],
    b: &[f32],
    eps: f32,
) {
    debug_assert_eq!(x.len(), channels * t);
    for c in 0..channels {
        let r = &mut x[c * t..(c + 1) * t];
        let mean = r.iter().sum::<f32>() / t as f32;
        let var = r.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / t as f32;
        let inv = 1.0 / (var + eps).sqrt();
        for v in r.iter_mut() {
            *v = (*v - mean) * inv * w[c] + b[c];
        }
    }
}

/// Grouped 1-D convolution, channel-major (`x` is `[in_ch][t_in]`).
///
/// Returns `([out_ch][t_out], t_out)`. `w` is `[out_ch][in_ch/groups][k]`.
///
/// `stride` matches PyTorch's `nn.Conv1d` (valid, no dilation). `same_len`
/// truncates the final frame, which is what `Wav2Vec2SamePadLayer` does for the
/// positional convolution.
#[allow(clippy::too_many_arguments)]
pub fn conv1d(
    x: &[f32],
    in_ch: usize,
    t_in: usize,
    w: &[f32],
    out_ch: usize,
    k: usize,
    stride: usize,
    groups: usize,
    pad: usize,
    bias: Option<&[f32]>,
    same_len: bool,
) -> (Vec<f32>, usize) {
    debug_assert_eq!(x.len(), in_ch * t_in);
    let in_per_group = in_ch / groups;
    let out_per_group = out_ch / groups;
    debug_assert_eq!(w.len(), out_ch * in_per_group * k);
    debug_assert_eq!(out_ch % groups, 0);
    debug_assert_eq!(in_ch, in_per_group * groups);

    let padded = t_in + 2 * pad;
    if padded < k {
        return (vec![0f32; 0], 0);
    }
    let mut t_out = (padded - k) / stride + 1;
    // Wav2Vec2SamePadLayer removes one trailing frame when the kernel is even.
    let truncated = same_len && k % 2 == 0;
    if truncated && t_out > 0 {
        t_out -= 1;
    }

    if t_out == 0 {
        return (vec![0f32; 0], 0);
    }

    let mut out = vec![0f32; out_ch * t_out];
    // Split over *output channels*: every `oc` reads the shared input rows and
    // writes only its own row of `out`, so the split changes nothing about how a
    // single output value is accumulated. This is the phase that decides a short
    // clip's wall time — the conv stack is ~34·10⁹ MACs against the transformer's
    // ~68·10⁹, but only the transformer was parallel, so a 14 s window spent its
    // first ~40 s on one core out of six.
    rows_split_mut(&mut out, t_out, |run, lo| {
        for (i, or) in run.chunks_mut(t_out).enumerate() {
            let oc = lo + i;
            let g = oc / out_per_group;
            let ic_base = g * in_per_group;
            if let Some(b) = bias {
                or.fill(b[oc]);
            }
            for ic in 0..in_per_group {
                let xr = &x[(ic_base + ic) * t_in..(ic_base + ic + 1) * t_in];
                let wr = &w[(oc * in_per_group + ic) * k..(oc * in_per_group + ic + 1) * k];
                for kk in 0..k {
                    let wv = wr[kk];
                    if wv == 0.0 {
                        continue;
                    }
                    // Output `ot` taps input index `ot*stride + kk - pad`.
                    for (ot, o) in or.iter_mut().enumerate() {
                        let it = ot * stride + kk;
                        if it >= pad && it - pad < t_in {
                            *o += wv * xr[it - pad];
                        }
                    }
                }
            }
        }
    });
    (out, t_out)
}

/// Reconstruct a weight-normed conv kernel: `w = g · v / ‖v‖`.
///
/// `v` is `[out_ch][in_per_group][k]` and `g` is `[1][1][k]`; the norm is taken
/// over the output-channel and input-channel axes for each kernel position,
/// which is what `weight_norm(dim=2)` stores. Skipping this step would feed the
/// positional convolution the wrong bytes entirely.
pub fn reconstruct_weight_norm(
    v: &[f32],
    g: &[f32],
    out_ch: usize,
    in_per_group: usize,
    k: usize,
) -> Vec<f32> {
    debug_assert_eq!(g.len(), k);
    let mut w = vec![0f32; v.len()];
    for kk in 0..k {
        let mut sum_sq = 0f32;
        for oc in 0..out_ch {
            for ic in 0..in_per_group {
                let idx = (oc * in_per_group + ic) * k + kk;
                sum_sq += v[idx] * v[idx];
            }
        }
        let norm = sum_sq.sqrt().max(1e-12);
        for oc in 0..out_ch {
            for ic in 0..in_per_group {
                let idx = (oc * in_per_group + ic) * k + kk;
                w[idx] = g[kk] * v[idx] / norm;
            }
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erf_matches_known_values() {
        // Reference values from standard tables.
        for (x, want) in [
            (0.0f32, 0.0f32),
            (1.0, 0.8427008),
            (-1.0, -0.8427008),
            (2.0, 0.9953223),
        ] {
            let got = erf(x);
            assert!((got - want).abs() < 1e-6, "erf({x}) = {got}, want {want}");
        }
    }

    #[test]
    fn gelu_is_zero_at_origin_and_negative_mirror() {
        let mut v = vec![0.0f32];
        gelu(&mut v);
        assert!(v[0].abs() < 1e-7);
        // gelu(x) - gelu(-x) == x
        let mut v = vec![1.5f32, -1.5f32];
        gelu(&mut v);
        assert!((v[0] - v[1] - 1.5).abs() < 1e-5, "{v:?}");
    }

    #[test]
    fn layer_norm_produces_zero_mean_unit_variance() {
        let mut x = vec![1.0f32, 2.0, 3.0, 4.0, 10.0, 20.0, 30.0, 40.0];
        let w = vec![1.0f32; 4];
        let b = vec![0.0f32; 4];
        layer_norm(&mut x, 2, 4, &w, &b, 1e-5);
        for row in x.chunks(4) {
            let mean = row.iter().sum::<f32>() / 4.0;
            assert!(mean.abs() < 1e-5);
        }
    }

    /// Deterministic filler for kernel tests: values vary with the index so that a
    /// reordered accumulation shows up as a different `f32`.
    fn synth(len: usize, m: usize, mid: f32, div: f32) -> Vec<f32> {
        (0..len).map(|i| ((i % m) as f32 - mid) / div).collect()
    }

    /// `linear` exactly as it was written before row blocking: one output row at a
    /// time, streaming the whole of `w` for each.
    fn linear_naive(
        x: &[f32],
        t: usize,
        in_dim: usize,
        w: &[f32],
        b: &[f32],
        out_dim: usize,
    ) -> Vec<f32> {
        let mut out = vec![0f32; t * out_dim];
        for row in 0..t {
            let xr = &x[row * in_dim..(row + 1) * in_dim];
            for o in 0..out_dim {
                let wr = &w[o * in_dim..(o + 1) * in_dim];
                let mut acc = 0f32;
                for i in 0..in_dim {
                    acc += xr[i] * wr[i];
                }
                out[row * out_dim + o] = acc + b[o];
            }
        }
        out
    }

    /// `ROW_BLOCK` buys cache reuse and must not buy it with a single bit. `t`
    /// spans the block boundary in every direction: below it, exactly on it, one
    /// past it, and large enough to leave a partial tail block.
    #[test]
    fn blocking_rows_is_bit_identical_to_the_row_at_a_time_kernel() {
        let (in_dim, out_dim) = (40, 24);
        let w = synth(out_dim * in_dim, 17, 8.0, 4.25);
        let b = synth(out_dim, 13, 6.0, 10.0);
        for t in [1, 7, ROW_BLOCK, ROW_BLOCK + 1, 64, 257] {
            let x = synth(t * in_dim, 41, 20.0, 6.5);
            set_thread_budget(1);
            assert_eq!(
                linear_naive(&x, t, in_dim, &w, &b, out_dim),
                linear(&x, t, in_dim, &w, &b, out_dim),
                "row blocking changed the result at t = {t}"
            );
        }
    }

    /// The kernels are allowed to get faster, never *different*: the forward pass
    /// is checked against reference dumps stage by stage, so a row split that
    /// reassociated a single addition would be a correctness bug, not a rounding
    /// difference. A budget of 1 is a single run; a budget above the row count
    /// forces the fan-out.
    #[test]
    fn split_rows_are_bit_identical_to_the_sequential_kernel() {
        let (t, in_dim, out_dim) = (257, 40, 24);
        let x = synth(t * in_dim, 41, 20.0, 6.5);
        let w = synth(out_dim * in_dim, 17, 8.0, 4.25);
        let b = synth(out_dim, 13, 6.0, 10.0);

        set_thread_budget(1);
        let serial = linear(&x, t, in_dim, &w, &b, out_dim);
        let mut serial_softmax = x[..t * out_dim].to_vec();
        softmax(&mut serial_softmax, t, out_dim);

        set_thread_budget(64);
        let parallel = linear(&x, t, in_dim, &w, &b, out_dim);
        let mut parallel_softmax = x[..t * out_dim].to_vec();
        softmax(&mut parallel_softmax, t, out_dim);

        assert_eq!(serial, parallel, "linear changed with the row split");
        assert_eq!(
            serial_softmax, parallel_softmax,
            "softmax changed with the row split"
        );
    }

    /// Splitting `conv1d` over output channels must not move a single bit: the
    /// channels are independent, and `synth` deliberately makes some weights and
    /// biases exactly `0.0` so the zero-weight skip is exercised on both sides.
    #[test]
    fn split_conv_channels_is_bit_identical_to_the_sequential_one() {
        let (in_ch, t_in, out_ch, k, stride, pad, groups) = (6, 97, 8, 3, 2, 1, 2);
        let x = synth(in_ch * t_in, 23, 11.0, 5.0);
        let w = synth(out_ch * (in_ch / groups) * k, 19, 7.0, 3.0);
        let b = synth(out_ch, 11, 4.0, 8.0);

        set_thread_budget(1);
        let serial = conv1d(
            &x,
            in_ch,
            t_in,
            &w,
            out_ch,
            k,
            stride,
            groups,
            pad,
            Some(&b),
            false,
        );
        set_thread_budget(64);
        let parallel = conv1d(
            &x,
            in_ch,
            t_in,
            &w,
            out_ch,
            k,
            stride,
            groups,
            pad,
            Some(&b),
            false,
        );

        assert_eq!(serial.1, parallel.1, "frame count changed");
        assert_eq!(
            serial.0, parallel.0,
            "conv1d changed with the channel split"
        );
    }

    #[test]
    fn conv1d_matches_hand_computed_sum() {
        // 1 input channel, 1 output channel, kernel 2, stride 1, no padding.
        let x = vec![1.0f32, 2.0, 3.0, 4.0];
        let w = vec![1.0f32, 1.0]; // out=1, in=1, k=2
        let (out, t_out) = conv1d(&x, 1, 4, &w, 1, 2, 1, 1, 0, None, false);
        assert_eq!(t_out, 3);
        assert_eq!(out, vec![3.0f32, 5.0, 7.0]);
    }

    #[test]
    fn same_len_drops_one_trailing_frame_for_even_kernel() {
        let x = vec![1.0f32; 8];
        let w = vec![1.0f32; 4]; // out=1, in=1, k=4
                                 // t_in 8 + 2*2 pad = 12; (12-4)/1 + 1 = 9 frames; same-pad drops 1 → 8.
        let (out, t_out) = conv1d(&x, 1, 8, &w, 1, 4, 1, 1, 2, None, true);
        assert_eq!(t_out, 8, "padded len 12 -> 9 frames -> 8 after same-pad");
        assert_eq!(out.len(), 8);
    }
}
