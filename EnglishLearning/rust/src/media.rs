//! Media core for `sentence-video` — pure-Rust media I/O on FFmpeg via
//! `ffmpeg-next`, in-process (no Python, no FFmpeg subprocess).
//!
//! Ported from the (previously Python) `english_media_native` extension and
//! `av_utils.py`. Functions:
//!   * `probe_duration`   — media duration in seconds, no `ffprobe`.
//!   * `decode_to_f32`    — decode + resample any audio/video stream to f32.
//!   * `write_wav`        — persist decoded samples as a RIFF/WAV file.
//!   * `decode_segment_wav`— decode a [start,end] slice of a file to a WAV.
//!   * `mux_video_audio`  — remux a video stream + (AAC-encoded) audio → .mp4.

use ffmpeg_next::format::flag::Flags as OutputFlags;
use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::format::Sample;
use ffmpeg_next::media::Type as MediaType;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::{codec, encoder, format, frame, packet};
use ffmpeg_next::error::EAGAIN;
use std::path::Path;

pub type Result<T> = std::result::Result<T, String>;

fn media_err(msg: &str) -> String {
    format!("sentence-video: {msg}")
}

fn ff_err(e: ffmpeg_next::Error) -> String {
    media_err(&e.to_string())
}

fn ff_input(path: &str) -> std::result::Result<ffmpeg_next::format::context::Input, ffmpeg_next::Error> {
    format::input(&std::path::PathBuf::from(path))
}

fn ff_output(path: &str) -> std::result::Result<ffmpeg_next::format::context::Output, ffmpeg_next::Error> {
    format::output(&std::path::PathBuf::from(path))
}

fn init() -> Result<()> {
    ffmpeg_next::init().map_err(ff_err)
}

// ─────────────────────────────────────────────────────────────────────────
// probe_duration
// ─────────────────────────────────────────────────────────────────────────

/// Duration in seconds of any media file (container duration, falling back to
/// the longest stream duration).
pub fn probe_duration(path: &str) -> Result<f64> {
    init()?;
    let input = ff_input(path).map_err(ff_err)?;
    let d = input.duration();
    if d > 0 {
        return Ok(d as f64 / 1_000_000.0);
    }
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

/// Native sample rate of the first audio stream in a file.
pub fn audio_sample_rate(path: &str) -> Result<u32> {
    init()?;
    let input = ff_input(path).map_err(ff_err)?;
    let stream = input
        .streams()
        .best(MediaType::Audio)
        .ok_or_else(|| media_err("no audio stream in input"))?;
    let ctx = codec::Context::from_parameters(stream.parameters()).map_err(ff_err)?;
    Ok(ctx.decoder().audio().map_err(ff_err)?.rate())
}

// ─────────────────────────────────────────────────────────────────────────
// decode_to_f32
// ─────────────────────────────────────────────────────────────────────────

/// Decode any audio/video file to a flat `Vec<f32>` (`S*C` samples, C channels,
/// concatenated plane order). `mono=true` collapses to a single channel.
pub fn decode_to_f32(
    path: &str,
    target_sr: u32,
    mono: bool,
    max_seconds: Option<f64>,
) -> Result<(Vec<f32>, u16)> {
    init()?;
    let mut input = ff_input(path).map_err(ff_err)?;
    let audio_stream = input
        .streams()
        .best(MediaType::Audio)
        .ok_or_else(|| media_err("no audio stream in input"))?;
    let index = audio_stream.index();

    let dec_ctx = codec::Context::from_parameters(audio_stream.parameters()).map_err(ff_err)?;
    let mut decoder = dec_ctx.decoder().audio().map_err(ff_err)?;

    let out_layout = if mono {
        ChannelLayout::MONO
    } else {
        ChannelLayout::STEREO
    };
    let out_fmt = Sample::F32(SampleType::Planar);

    // Some decoders (e.g. PCM WAV) report an empty/unspecified channel
    // layout; fall back to a default derived from the channel count.
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

    let push = |rf: &frame::Audio, out: &mut Vec<f32>| {
        if rf.samples() == 0 {
            return;
        }
        for c in 0..rf.planes().min(channels as usize) {
            out.extend_from_slice(rf.plane::<f32>(c));
        }
    };

    let out_cap = |src_samples: usize, src_rate: u32| -> usize {
        let rate = src_rate.max(1);
        ((src_samples as u64 * target_sr as u64 + rate as u64 - 1) / rate as u64) as usize + 32
    };
    let make_out = |src: &frame::Audio| -> frame::Audio {
        frame::Audio::new(out_fmt, out_cap(src.samples(), src.rate()), out_layout)
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
                    f.set_channel_layout(src_layout);
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
                    break;
                }
                Err(e) => return Err(ff_err(e)),
            }
        }
        if done {
            break;
        }
    }

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

// ─────────────────────────────────────────────────────────────────────────
// WAV output
// ─────────────────────────────────────────────────────────────────────────

/// Write interleaved f32 samples to a 16-bit PCM RIFF/WAV file.
pub fn write_wav(path: &str, interleaved: &[f32], sample_rate: u32, channels: u16) -> Result<()> {
    use std::io::Write;
    let n = interleaved.len() as u32;
    let data_len = n * 2; // 16-bit
    let mut b = Vec::with_capacity(44 + data_len as usize);

    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVE");

    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * 2;
    b.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels * 2;
    b.extend_from_slice(&block_align.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for &s in interleaved {
        let sample = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        b.extend_from_slice(&sample.to_le_bytes());
    }

    let mut file = std::fs::File::create(path).map_err(|e| media_err(&e.to_string()))?;
    file.write_all(&b).map_err(|e| media_err(&e.to_string()))?;
    Ok(())
}

/// Decode the `[start_sec, end_sec]` slice of a media file and write it as a
/// WAV to `out_path`. Mono, `sample_rate` Hz.
pub fn decode_segment_wav(
    path: &str,
    start_sec: f64,
    end_sec: f64,
    sample_rate: u32,
    out_path: &str,
) -> Result<()> {
    let duration = (end_sec - start_sec).max(0.0);
    // Slice via the max_seconds budget, then offset into the sample stream.
    let (samples, ch) = decode_to_f32(path, sample_rate, true, None)?;
    let sr = sample_rate as f64;
    let skip = (start_sec * sr) as usize;
    let take = (duration * sr) as usize;
    if skip >= samples.len() {
        return write_wav(out_path, &[0.0; 0], sample_rate, ch);
    }
    let end = (skip + take).min(samples.len());
    let slice = &samples[skip..end];
    write_wav(out_path, slice, sample_rate, ch)
}

// ─────────────────────────────────────────────────────────────────────────
// mux_video_audio
// ─────────────────────────────────────────────────────────────────────────

/// Remux the video stream verbatim and encode a mono audio track to AAC into
/// an .mp4. Mirrors: `ffmpeg -i video -i audio -c:v copy -c:a aac -b:a 128k
/// -map 0:v:0 -map 1:a:0 -shortest out.mp4`.
pub fn mux_video_audio(
    video_path: &str,
    audio_path: &str,
    output_path: &str,
    audio_bitrate: usize,
    shortest: bool,
) -> Result<()> {
    init()?;

    let mut ictx = ff_input(video_path).map_err(ff_err)?;
    let vstream = ictx
        .streams()
        .best(MediaType::Video)
        .ok_or_else(|| media_err(&format!("no video stream in {video_path}")))?;
    let vstream_index = vstream.index();
    let vstream_time_base = vstream.time_base();
    let container_duration_us = ictx.duration();

    let sr = audio_sample_rate(audio_path)?;
    let (mut audio, _ch) = decode_to_f32(audio_path, sr, true, None)?;

    if shortest && container_duration_us > 0 {
        let max_samples = (container_duration_us as f64 / 1_000_000.0 * sr as f64) as usize;
        if audio.len() > max_samples {
            audio.truncate(max_samples);
        }
    }

    let mut octx = ff_output(output_path).map_err(ff_err)?;
    let global = octx
        .format()
        .flags()
        .contains(OutputFlags::GLOBAL_HEADER);

    // Video stream: copy (no transcode).
    let mut vout = octx.add_stream(encoder::find(codec::Id::None)).map_err(ff_err)?;
    vout.set_parameters(vstream.parameters());
    unsafe {
        (*vout.parameters().as_mut_ptr()).codec_tag = 0;
    }
    vout.set_time_base(vstream_time_base);
    let vout_index = vout.index();

    // Audio stream: AAC.
    let aac = encoder::find(codec::Id::AAC).ok_or_else(|| media_err("AAC encoder not found"))?;
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

    for (stream, mut pkt) in ictx.packets() {
        if stream.index() != vstream_index {
            continue;
        }
        pkt.rescale_ts(stream.time_base(), vstream_time_base);
        pkt.set_position(-1);
        pkt.set_stream(vout_index);
        pkt.write_interleaved(&mut octx).map_err(ff_err)?;
    }

    let frame_size = aenc.frame_size().max(1) as usize;
    let encode_and_mux =
        |aenc: &mut codec::encoder::Audio, octx: &mut format::context::Output| -> Result<()> {
            let mut encoded = packet::Packet::empty();
            while aenc.receive_packet(&mut encoded).is_ok() {
                encoded.set_stream(aout_index);
                encoded.rescale_ts((1, sr as i32), aout_time_base);
                encoded.write_interleaved(octx).map_err(ff_err)?;
            }
            Ok(())
        };

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

    aenc.send_eof().map_err(ff_err)?;
    let mut encoded = packet::Packet::empty();
    while aenc.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(aout_index);
        encoded.rescale_ts((1, sr as i32), aout_time_base);
        encoded.write_interleaved(&mut octx).map_err(ff_err)?;
    }

    octx.write_trailer().map_err(ff_err)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────

/// Expand `~/` and make the path absolute relative to `cwd`.
pub fn normalize_path(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_string_lossy().into_owned()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(p).to_string_lossy().into_owned()
    } else {
        p.to_string_lossy().into_owned()
    }
}