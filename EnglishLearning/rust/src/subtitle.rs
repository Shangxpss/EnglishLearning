//! Pure-Rust `.srt` / `.vtt` subtitle parser → `Vec<Cue>`.

use crate::models::{Cue, Word};

fn parse_ts(ts: &str) -> Option<f64> {
    // Accepts HH:MM:SS(.fff or ,fff) and MM:SS formats. Separator `,` or `.`.
    let ts = ts.trim();
    let sep = if ts.contains(',') { ',' } else { '.' };
    let mut it = ts.split(sep);
    let time_part = it.next()?.trim();
    let frac: f64 = it.next().unwrap_or("0").parse().unwrap_or(0.0) / 1000.0;

    let nums: Vec<f64> = time_part
        .split(':')
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    let secs = match nums.len() {
        3 => nums[0] * 3600.0 + nums[1] * 60.0 + nums[2],
        2 => nums[0] * 60.0 + nums[1],
        1 => nums[0],
        _ => return None,
    };
    Some(secs + frac)
}

/// Detect the subtitle type from the filename/extension.
pub fn detect_format(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".vtt") {
        "vtt"
    } else {
        "srt"
    }
}

/// Parse a subtitle file (`.srt` or `.vtt`) into cues (0-based index).
pub fn parse_subtitle(path: &str) -> Result<Vec<Cue>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read subtitle {path}: {e}"))?;
    // Normalize CRLF and strip a UTF-8 BOM.
    let text = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");

    let fmt = detect_format(path);
    // WebVTT has a header (`WEBVTT`) and possibly cue settings after the
    // timing (`align:start position:...`). Strip the trailing settings from
    // the time line before parsing.
    let is_vtt = fmt == "vtt";

    let mut cues: Vec<Cue> = Vec::new();

    let blocks = text.split("\n\n");
    for block in blocks {
        let lines: Vec<&str> = block.lines().map(|l| l.trim()).collect();
        if lines.is_empty() {
            continue;
        }

        // Skip the leading WEBVTT header block (and any metadata).
        let mut start = 0usize;
        if is_vtt && lines.get(0).is_some_and(|l| l.eq_ignore_ascii_case("WEBVTT")) {
            start = 1;
        }

        let mut time_line: Option<usize> = None;
        for i in start..lines.len() {
            if lines[i].contains("-->") {
                time_line = Some(i);
                break;
            }
        }
        let Some(tl) = time_line else { continue };

        let (s, e) = parse_time_line(lines[tl])?;
        let text_lines: Vec<&str> = lines[tl + 1..]
            .iter()
            .copied()
            .filter(|l| !l.is_empty())
            .collect();
        if text_lines.is_empty() {
            continue;
        }
        let text = text_lines.join(" ");
        let words: Vec<Word> = text
            .split_whitespace()
            .map(|w| Word {
                text: w.to_string(),
                start: 0.0,
                end: 0.0,
                score: 1.0,
            })
            .collect();

        cues.push(Cue {
            index: cues.len(),
            start: s,
            end: e,
            text,
            words,
            source_path: None,
        });
    }

    if cues.is_empty() {
        return Err(format!("no cues parsed from {path}"));
    }
    Ok(cues)
}

/// Parse a `start --> end` timing line, ignoring trailing WebVTT cue settings.
fn parse_time_line(line: &str) -> Result<(f64, f64), String> {
    let arrow_idx = line
        .find("-->")
        .ok_or_else(|| format!("invalid timing line: {line}"))?;
    let head = &line[..arrow_idx];
    // VTT cue settings follow the end timestamp and are space-separated.
    let tail_full = &line[arrow_idx + 3..];
    let end_ts = tail_full.split_whitespace().next().unwrap_or_default();

    let start = parse_ts(head).ok_or_else(|| format!("invalid start time: {head}"))?;
    let end = parse_ts(end_ts).ok_or_else(|| format!("invalid end time: {end_ts}"))?;
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_timestamp() {
        assert!((parse_ts("00:01:02,500").unwrap() - 62.5).abs() < 1e-6);
        assert!((parse_ts("00:01:02.500").unwrap() - 62.5).abs() < 1e-6);
        assert!((parse_ts("01:02").unwrap() - 62.0).abs() < 1e-6);
    }

    #[test]
    fn webvtt_timing_with_settings() {
        let (s, e) = parse_time_line("00:00:01.000 --> 00:00:03.500 align:start").unwrap();
        assert!((s - 1.0).abs() < 1e-6);
        assert!((e - 3.5).abs() < 1e-6);
    }
}