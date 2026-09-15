//! Rust ffmpeg-next media helpers for the English Learning backend.
//!
//! Mirrors the behaviour of `app/services/sync/av_utils.py`, which previously
//! used the bundled-FFmpeg Python library `PyAV`. All media I/O here is
//! pure-Rust against the system FFmpeg libraries through `ffmpeg-next`,
//! exposed to Python via PyO3.
//!
//! Ported functions:
//!   * `duration`       → `av_utils.get_media_duration`
//!   * `decode_audio`   → `av_utils.decode_audio` (decode + resample → f32)
//!   * `mux_video_audio`→ `av_utils.mux_video_audio` (remux video + AAC audio)

use ffmpeg_next::format::flag::Flags as OutputFlags;
use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::media::Type as MediaType;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::format::Sample;
use ffmpeg_next::{codec, encoder, format, frame, packet};
use ffmpeg_next::error::EAGAIN;
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

    // F32 planar → concatenated plane order. For mono this is the single
    // plane; for stereo it is all of L then all of R — matching PyAV's
    // `to_ndarray().reshape(-1)` on a planar frame.
    let push = |rf: &frame::Audio, out: &mut Vec<f32>| {
        if rf.samples() == 0 {
            return;
        }
        for c in 0..rf.planes().min(channels as usize) {
            out.extend_from_slice(rf.plane::<f32>(c));
        }
    };

    // swr_convert_frame (used by Context::run) writes into a caller-supplied
    // frame; if it is too small it returns EAGAIN, so size it with the
    // up-sampled length (rounded up) plus a small margin for swr's own
    // internal buffering against fractional sample-ratio drift.
    let out_cap = |src_samples: usize, src_rate: u32| -> usize {
        let rate = src_rate.max(1);
        ((src_samples as u64 * target_sr as u64 + rate as u64 - 1) / rate as u64) as usize + 32
    };
    let make_out = |src: &frame::Audio| -> frame::Audio {
        frame::Audio::new(
            out_fmt,
            out_cap(src.samples(), src.rate()),
            out_layout,
        )
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
                    // Some decoders (e.g. PCM WAV) leave the decoded frame's
                    // channel layout empty (0x0), which makes the frame-based
                    // swr_convert_frame reject it as "Input changed". Pin the
                    // frame layout to the one we configured the resampler with.
                    f.set_channel_layout(src_layout);
                    // run() converts the whole input frame in one call; any
                    // fractional remainder is buffered internally and only
                    // drained by the final flush() below.
                    resampler.run(&f, &mut rf).map_err(ff_err)?;
                    push(&rf, &mut out);
                    if let Some(m) = max_samples {
                        if out.len() >= m * channels as usize {
                            out.truncate(m * channels as usize);
                            done = true;
                            break;
                        }
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == EAGAIN =>
                {
                    // Decoder needs another input packet; move on.
                    break;
                }
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
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == EAGAIN =>
                {
                    break;
                }
                Err(e) => return Err(ff_err(e)),
            }
        }
        // Drain the resampler's internally buffered residual output. flush()
        // returns Ok(None) when no buffered delay remains; that last call may
        // still have produced samples, so push before breaking.
        let flush_cap = (target_sr as usize / 10).max(4096);
        loop {
            let mut rf = frame::Audio::new(out_fmt, flush_cap, out_layout);
            match resampler.flush(&mut rf) {
                Ok(None) => {
                    push(&rf, &mut out);
                    break;
                }
                Ok(Some(_)) => push(&rf, &mut out),
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
// mux_video_audio
// ─────────────────────────────────────────────────────────────────────────

/// Reports the native sample rate of the first audio stream in a file.
fn audio_sample_rate(path: &str) -> PyResult<u32> {
    let input = format::input(path).map_err(ff_err)?;
    let stream = input
        .streams()
        .best(MediaType::Audio)
        .ok_or_else(|| PyValueError::new_err("no audio stream in input"))?;
    let ctx = codec::Context::from_parameters(stream.parameters()).map_err(ff_err)?;
    Ok(ctx.decoder().audio().map_err(ff_err)?.rate())
}

/// Mirrors `av_utils.mux_video_audio`. Remuxes the video stream verbatim and
/// encodes a mono WAV audio track to AAC into an .mp4.
///
/// This is the Rust replacement for the system-ffmpeg command::
///
///     ffmpeg -i video.mp4 -i audio.wav \
///            -c:v copy -c:a aac -b:a 128k \
///            -map 0:v:0 -map 1:a:0 -shortest out.mp4
#[pyfunction]
#[pyo3(signature = (video_path, audio_path, output_path, audio_bitrate=128_000, shortest=true))]
fn mux_video_audio(
    video_path: &str,
    audio_path: &str,
    output_path: &str,
    audio_bitrate: usize,
    shortest: bool,
) -> PyResult<()> {
    init()?;

    // ── open video input ───────────────────────────────────────────────
    let mut ictx = format::input(video_path).map_err(ff_err)?;
    let vstream = ictx
        .streams()
        .best(MediaType::Video)
        .ok_or_else(|| PyValueError::new_err(format!("no video stream in {video_path}")))?;
    let vstream_index = vstream.index();
    let vstream_time_base = vstream.time_base();
    // Container duration in microseconds (AV_TIME_BASE).
    let container_duration_us = ictx.duration();

    // ── audio: sample rate + flat mono f32 at the native rate ──────────
    let sr = audio_sample_rate(audio_path)?;
    let (mut audio, _ch) = decode_to_f32(audio_path, sr, true, None)?;

    // Emulate -shortest: clip audio to the video duration.
    if shortest && container_duration_us > 0 {
        let max_samples = (container_duration_us as f64 / 1_000_000.0 * sr as f64) as usize;
        if audio.len() > max_samples {
            audio.truncate(max_samples);
        }
    }

    // ── open output container ──────────────────────────────────────────
    let mut octx = format::output(output_path).map_err(ff_err)?;
    let global = octx
        .format()
        .flags()
        .contains(OutputFlags::GLOBAL_HEADER);

    // Video stream: remux (codec config copied verbatim, packets pass
    // through without transcode → the `-c:v copy` behaviour).
    let mut vout = octx
        .add_stream(encoder::find(codec::Id::None))
        .map_err(ff_err)?;
    vout.set_parameters(vstream.parameters());
    // Reset codec_tag so the target container (mp4) can pick its own tag.
    unsafe {
        (*vout.parameters().as_mut_ptr()).codec_tag = 0;
    }
    // Keep the input stream time base so packet timestamps map directly.
    vout.set_time_base(vstream_time_base);
    let vout_index = vout.index();

    // Audio stream: AAC encoder.
    let aac = encoder::find(codec::Id::AAC)
        .ok_or_else(|| media_err("AAC encoder not found"))?;
    let mut aout = octx.add_stream(aac).map_err(ff_err)?;
    let actx = codec::Context::from_parameters(aout.parameters()).map_err(ff_err)?;
    let mut aenc = actx.encoder().audio().map_err(ff_err)?;

    if global {
        aenc.set_flags(ffmpeg_next::codec::flag::Flags::GLOBAL_HEADER);
    }
    aenc.set_rate(sr as i32);
    aenc.set_channel_layout(ChannelLayout::MONO);
    aenc.set_channels(1);
    aenc.set_format(Sample::F32(SampleType::Planar));
    aenc.set_bit_rate(audio_bitrate);
    aenc.set_time_base((1, sr as i32));
    aout.set_time_base((1, sr as i32));
    let aout_index = aout.index();

    let mut aenc = aenc.open_as(aac).map_err(ff_err)?;
    aout.set_parameters(&aenc);

    octx.write_header().map_err(ff_err)?;

    let aout_time_base = octx.stream(aout_index).map(|s| s.time_base()).unwrap();

    // ── 1. remux video packets (copy, no decode/encode) ────────────────
    for (stream, mut pkt) in ictx.packets() {
        if stream.index() != vstream_index {
            continue;
        }
        pkt.rescale_ts(stream.time_base(), vstream_time_base);
        pkt.set_position(-1);
        pkt.set_stream(vout_index);
        pkt.write_interleaved(&mut octx).map_err(ff_err)?;
    }

    // ── 2. encode audio frames → AAC packets ──────────────────────────
    let frame_size = aenc.frame_size().max(1) as usize;
    let encode_and_mux =
        |aenc: &mut codec::encoder::Audio, octx: &mut format::context::Output| -> PyResult<()> {
            let mut encoded = packet::Packet::empty();
            while aenc.receive_packet(&mut encoded).is_ok() {
                encoded.set_stream(aout_index);
                encoded.rescale_ts((1, sr as i32), aout_time_base);
                encoded.write_interleaved(octx).map_err(ff_err)?;
            }
            Ok(())
        };

    // Feed fixed-size frames (padded) so the AAC encoder always receives a
    // full block, matching PyAV's chunking + zero-pad of the final block.
    let n_frames = audio.len().div_ceil(frame_size);
    for frame_index in 0..n_frames {
        let start = frame_index * frame_size;
        let end = (start + frame_size).min(audio.len());
        let mut chunk = audio[start..end].to_vec();
        chunk.resize(frame_size, 0.0f32);

        let mut af = frame::Audio::new(
            Sample::F32(SampleType::Planar),
            frame_size,
            ChannelLayout::MONO,
        );
        af.set_rate(sr);
        af.set_channel_layout(ChannelLayout::MONO);
        af.set_pts(Some((frame_index * frame_size) as i64));
        af.plane_mut::<f32>(0).copy_from_slice(&chunk);
        aenc.send_frame(&af).map_err(ff_err)?;
        encode_and_mux(&mut aenc, &mut octx)?;
    }

    // Flush the audio encoder (emits trailing buffered packets).
    aenc.send_eof().map_err(ff_err)?;
    let mut encoded = packet::Packet::empty();
    while aenc.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(aout_index);
        encoded.rescale_ts((1, sr as i32), aout_time_base);
        encoded.write_interleaved(&mut octx).map_err(ff_err)?;
    }

    // ── finalize ──────────────────────────────────────────────────────
    octx.write_trailer().map_err(ff_err)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Python module registration
// ─────────────────────────────────────────────────────────────────────────

#[pymodule]
fn english_media_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(duration, m)?)?;
    m.add_function(wrap_pyfunction!(decode_audio, m)?)?;
    m.add_function(wrap_pyfunction!(mux_video_audio, m)?)?;
    Ok(())
}