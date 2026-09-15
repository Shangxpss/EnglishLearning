//! Rust ffmpeg-next media helpers for the English Learning backend.
//!
//! Mirrors the behaviour of `app/services/sync/av_utils.py`, which previously
//! used the bundled-FFmpeg Python library `PyAV`. All media I/O here is
//! pure-Rust against the system FFmpeg libraries through `ffmpeg-next`,
//! exposed to Python via PyO3.
//!
//! Ported functions:
//!   * `duration`     → `av_utils.get_media_duration`
//!   * `decode_audio` → `av_utils.decode_audio` (decode + resample → f32)

use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::media::Type as MediaType;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::format::Sample;
use ffmpeg_next::{codec, format, frame};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

fn media_err(msg: &str) -> PyErr {
    PyRuntimeError::new_err(format!("english_media_native: {msg}"))
}

fn ff_err(e: ffmpeg_next::Error) -> PyErr {
    media_err(&e.to_string())
}

fn init() -> PyResult<()> {
    ffmpeg_next::init().map_err(ff_err)
}

// ─────────────────────────────────────────────────────────────────────────
// duration
// ─────────────────────────────────────────────────────────────────────────

/// Mirrors `av_utils.get_media_duration`. Returns duration in seconds.
#[pyfunction]
fn duration(path: &str) -> PyResult<f64> {
    init()?;
    let input = format::input(path).map_err(ff_err)?;
    // Container duration is in AV_TIME_BASE units (microseconds).
    let d = input.duration();
    if d > 0 {
        return Ok(d as f64 / 1_000_000.0);
    }
    // Fallback: longest stream duration in its own time_base.
    let mut best = 0.0f64;
    for s in input.streams() {
        let dur = s.duration();
        if dur < 0 {
            continue;
        }
        let tb = s.time_base();
        let secs = dur as f64 * f64::from(tb.numerator()) / f64::from(tb.denominator());
        if secs > best {
            best = secs;
        }
    }
    Ok(best)
}

// ─────────────────────────────────────────────────────────────────────────
// decode_audio
// ─────────────────────────────────────────────────────────────────────────

/// Mirrors `av_utils.decode_audio`. Decodes any audio/video file to a flat
/// `Vec<f32>` (M samples for mono, otherwise S*2 interleaved stereo).
fn decode_to_f32(
    path: &str,
    target_sr: u32,
    mono: bool,
    max_seconds: Option<f64>,
) -> PyResult<(Vec<f32>, u16)> {
    init()?;
    let mut input = format::input(path).map_err(ff_err)?;

    let audio_stream = input
        .streams()
        .best(MediaType::Audio)
        .ok_or_else(|| PyValueError::new_err("no audio stream in input"))?;
    let index = audio_stream.index();

    let dec_ctx = codec::Context::from_parameters(audio_stream.parameters()).map_err(ff_err)?;
    let mut decoder = dec_ctx.decoder().audio().map_err(ff_err)?;

    let out_layout = if mono {
        ChannelLayout::MONO
    } else {
        ChannelLayout::STEREO
    };
    let out_fmt = Sample::F32(SampleType::Planar);

    // Some decoders report an empty/unspecified channel layout (e.g. PCM).
    // Fall back to a default layout derived from the channel count.
    let in_layout = decoder.channel_layout();
    let src_layout = if in_layout.is_empty() {
        ChannelLayout::default(decoder.channels() as i32)
    } else {
        in_layout
    };

    let mut resampler = resampling::Context::get(
        decoder.format(),
        src_layout,
        decoder.rate(),
        out_fmt,
        out_layout,
        target_sr,
    )
    .map_err(ff_err)?;

    let max_samples = max_seconds.map(|s| (s * target_sr as f64) as usize);
    let channels = out_layout.channels() as u16;
    let mut out: Vec<f32> = Vec::new();

    // F32 planar → interleaved f32 (for mono this is just the single plane).
    let push = |rf: &frame::Audio, out: &mut Vec<f32>| {
        if rf.samples() == 0 {
            return;
        }
        for c in 0..rf.channels() as usize {
            out.extend_from_slice(rf.plane::<f32>(c));
        }
    };

    // swr_convert_frame (used by Context::run) needs an output frame that is
    // pre-allocated with enough samples for the rescaled length, otherwise it
    // returns EAGAIN. Allocate once with a generous capacity per source frame.
    let make_out = |src: &frame::Audio| -> frame::Audio {
        let in_sr = src.sample_rate().max(1);
        let out_samples = (src.samples() as i64 * target_sr as i64 / in_sr as i64).max(0) as usize;
        frame::Audio::new(out_fmt, out_samples, out_layout)
    };

    let mut done = false;
    for (stream, pkt) in input.packets() {
        if stream.index() != index {
            continue;
        }
        if decoder.send_packet(&pkt).is_err() {
            continue;
        }
        loop {
            let mut f = frame::Audio::empty();
            match decoder.receive_frame(&mut f) {
                Ok(()) => {
                    let mut rf = make_out(&f);
                    if resampler.run(&f, &mut rf).is_ok() {
                        push(&rf, &mut out);
                    }
                    // Each swr.run returns at most the allocated output; but
                    // with buffering there may be more. Drain residual in place.
                    loop {
                        let mut extra = frame::Audio::empty();
                        match resampler.run(&mut extra_source(), &mut extra) {
                            Ok(_) => push(&extra, &mut out),
                            _ => break,
                        }
                    }
                    if let Some(m) = max_samples {
                        if out.len() >= m * channels as usize {
                            out.truncate(m * channels as usize);
                            done = true;
                            break;
                        }
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(e) => return Err(ff_err(e)),
            }
        }
        if done {
            break;
        }
    }

    // Flush decoder.
    if !done {
        let _ = decoder.send_eof();
        loop {
            let mut f = frame::Audio::empty();
            match decoder.receive_frame(&mut f) {
                Ok(()) => {
                    let mut rf = make_out(&f);
                    if resampler.run(&f, &mut rf).is_ok() {
                        push(&rf, &mut out);
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(e) => return Err(ff_err(e)),
            }
        }
        // Flush resampler residual.
        loop {
            let mut rf = frame::Audio::empty();
            match resampler.flush(&mut rf) {
                Ok(_) => push(&rf, &mut out),
                Err(_) => break,
            }
        }
    }

    if let Some(m) = max_samples {
        out.truncate(m * channels as usize);
    }
    Ok((out, channels))
}

/// Python binding: returns interleaved f32 samples as a flat list.
#[pyfunction]
#[pyo3(signature = (path, target_sr=24000, mono=true, max_seconds=None))]
fn decode_audio(
    path: &str,
    target_sr: u32,
    mono: bool,
    max_seconds: Option<f64>,
) -> PyResult<Vec<f32>> {
    decode_to_f32(path, target_sr, mono, max_seconds).map(|(s, _)| s)
}

// ─────────────────────────────────────────────────────────────────────────
// Python module registration
// ─────────────────────────────────────────────────────────────────────────

#[pymodule]
fn english_media_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(duration, m)?)?;
    m.add_function(wrap_pyfunction!(decode_audio, m)?)?;
    Ok(())
}