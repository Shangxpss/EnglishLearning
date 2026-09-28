//! Minimal reader for the `safetensors` format.
//!
//! Format: 8-byte little-endian `u64` header length, then that many bytes of
//! JSON metadata, then the raw tensor blob. Each tensor's `data_offsets` are
//! relative to the start of the blob.
//!
//! Why not a crate: the format is trivial (the header is JSON and the payload
//! is a flat byte range), and this keeps the `asr-align` feature free of new
//! dependencies.
//!
//! Tensors are read straight from the file into `Vec<f32>`. That is deliberate:
//! the blob is not guaranteed to be 4-byte aligned (this model's data section
//! starts at byte 25693, which is odd), so reinterpreting whole-file bytes as
//! `f32` would be misaligned. Reading per tensor into an aligned buffer avoids
//! both that and holding the 378 MB file in memory alongside the weights.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// One tensor's location and shape.
#[derive(Debug, Clone)]
struct Entry {
    shape: Vec<usize>,
    /// Byte offset of the payload, relative to the data section.
    begin: usize,
    end: usize,
}

/// An opened safetensors file.
pub struct Safetensors {
    file: File,
    /// Absolute byte offset of the data section within the file.
    data_start: u64,
    entries: HashMap<String, Entry>,
}

impl Safetensors {
    /// Open a file and parse its header.
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let mut file =
            File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;

        let mut len_bytes = [0u8; 8];
        file.read_exact(&mut len_bytes)
            .map_err(|e| format!("{}: truncated header length: {e}", path.display()))?;
        let header_len = u64::from_le_bytes(len_bytes);

        // A corrupt length would try to allocate wildly; 256 MiB is far above
        // any real header (this model's is ~25 KiB).
        if header_len == 0 || header_len > 256 * 1024 * 1024 {
            return Err(format!(
                "{}: implausible safetensors header length {header_len}",
                path.display()
            ));
        }

        let mut header = vec![0u8; header_len as usize];
        file.read_exact(&mut header)
            .map_err(|e| format!("{}: truncated header: {e}", path.display()))?;
        let data_start = 8 + header_len;

        let entries = parse_header(&String::from_utf8_lossy(&header))?;
        Ok(Self {
            file,
            data_start,
            entries,
        })
    }

    /// Shape of a tensor, for building the model and for diagnostics.
    pub fn shape(&self, name: &str) -> Result<&[usize], String> {
        self.entries
            .get(name)
            .map(|e| e.shape.as_slice())
            .ok_or_else(|| format!("tensor `{name}` is missing from the model file"))
    }

    /// Read a tensor as `f32`, validating the element count against `shape`.
    pub fn f32(&mut self, name: &str) -> Result<Vec<f32>, String> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| format!("tensor `{name}` is missing from the model file"))?
            .clone();
        let count = entry.shape.iter().product::<usize>();
        let bytes = entry.end - entry.begin;
        if bytes != count * 4 {
            return Err(format!(
                "tensor `{name}`: {} bytes does not match shape {:?} (expected {} bytes)",
                bytes,
                entry.shape,
                count * 4
            ));
        }

        self.file
            .seek(SeekFrom::Start(self.data_start + entry.begin as u64))
            .map_err(|e| format!("tensor `{name}`: seek failed: {e}"))?;

        // Read into an aligned `f32` buffer by filling it through its byte view.
        let mut out = vec![0f32; count];
        let buf = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, count * 4) };
        self.file
            .read_exact(buf)
            .map_err(|e| format!("tensor `{name}`: read failed: {e}"))?;
        Ok(out)
    }
}

/// Parse the JSON header into name → location.
///
/// Hand-rolled rather than `serde_json` so the module stays dependency-free and
/// works while `serde_json` is only present under another feature.
fn parse_header(json: &str) -> Result<HashMap<String, Entry>, String> {
    let mut out = HashMap::new();
    // Every tensor object has the shape `"name":{"dtype":"F32","shape":[..],"data_offsets":[a,b]}`.
    // Walking it with a scanner keeps this small and avoids a full JSON parser.
    let bytes = json.as_bytes();
    let mut i = 0usize;

    while let Some(name_start) = find(bytes, b'"', i) {
        // Key is a quoted string at the top level of the object.
        let Some(name_end) = find(bytes, b'"', name_start + 1) else {
            break;
        };
        let key = &json[name_start + 1..name_end];
        i = name_end + 1;

        // Only tensor entries have an object value; "__metadata__" has a string.
        let Some(colon) = find(bytes, b':', i) else {
            break;
        };
        let mut j = colon + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'{' {
            continue;
        }

        let Some(obj_end) = matching_brace(bytes, j) else {
            break;
        };
        let obj = &json[j..=obj_end];

        // Not every object is a tensor: this model carries
        // `"__metadata__":{"format":"pt"}`, which has no tensor data.
        if !obj.contains("\"data_offsets\"") {
            i = obj_end + 1;
            continue;
        }

        let shape = parse_usize_list(obj, "\"shape\"")?;
        let offsets = parse_usize_list(obj, "\"data_offsets\"")?;
        if offsets.len() != 2 {
            return Err(format!("tensor `{key}`: malformed data_offsets in header"));
        }
        // The only dtype this workload needs; fail loudly rather than misread.
        if !obj.contains("\"dtype\":\"F32\"") && !obj.contains("\"dtype\": \"F32\"") {
            return Err(format!(
                "tensor `{key}`: unsupported dtype (only F32 is supported)"
            ));
        }

        out.insert(
            key.to_string(),
            Entry {
                shape,
                begin: offsets[0],
                end: offsets[1],
            },
        );
        i = obj_end + 1;
    }

    if out.is_empty() {
        return Err("safetensors header contained no tensors".to_string());
    }
    Ok(out)
}

fn find(bytes: &[u8], needle: u8, from: usize) -> Option<usize> {
    bytes[from.min(bytes.len())..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

/// Index of the `}` closing the `{` at `start`, ignoring nested braces.
fn matching_brace(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read the `[1, 2, 3]` array following `key` inside `obj`.
fn parse_usize_list(obj: &str, key: &str) -> Result<Vec<usize>, String> {
    let at = obj
        .find(key)
        .ok_or_else(|| format!("header entry is missing {key}"))?;
    let open = obj[at..]
        .find('[')
        .ok_or_else(|| format!("header entry {key} has no array"))?
        + at;
    let close = obj[open..]
        .find(']')
        .ok_or_else(|| format!("header entry {key} has no closing bracket"))?
        + open;

    obj[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<usize>()
                .map_err(|e| format!("header entry {key}: bad number `{s}`: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_tensor_alongside_a_metadata_entry() {
        // `facebook/wav2vec2-base-960h` really ships `"__metadata__"` first, so
        // the parser must not mistake it for a tensor lacking `shape`.
        let header = r#"{"__metadata__":{"format":"pt"},"w":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#;
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&1.5f32.to_le_bytes());
        bytes.extend_from_slice(&(-2.0f32).to_le_bytes());

        let path = std::env::temp_dir().join("sentence-video-safetensors-test.st");
        std::fs::write(&path, &bytes).unwrap();

        let mut st = Safetensors::open(&path).unwrap();
        assert_eq!(st.shape("w").unwrap(), &[2]);
        assert_eq!(st.f32("w").unwrap(), vec![1.5f32, -2.0]);
        assert!(st.f32("__metadata__").is_err());
        let _ = std::fs::remove_file(&path);
    }
}
