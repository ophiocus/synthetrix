//! Extract embedded generation metadata from PNG originals.
//!
//! ComfyUI stores the graph in tEXt chunks keyed `workflow` (UI graph) and
//! `prompt` (API graph). A1111/Forge store a single `parameters` chunk.
//! JPEG/WebP originals re-encoded by CivitAI carry nothing recoverable.

use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

const PNG_SIG: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Parse tEXt / zTXt / iTXt chunks into {keyword: text}.
pub fn text_chunks(data: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if data.len() < 8 || &data[..8] != PNG_SIG {
        return out;
    }
    let mut pos = 8usize;
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let ctype = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(len);
        if body_end > data.len() {
            break;
        }
        let body = &data[body_start..body_end];
        match ctype {
            b"tEXt" => {
                if let Some(i) = body.iter().position(|&b| b == 0) {
                    let kw = String::from_utf8_lossy(&body[..i]).into_owned();
                    let txt = String::from_utf8_lossy(&body[i + 1..]).into_owned();
                    out.insert(kw, txt);
                }
            }
            b"zTXt" => {
                if let Some(i) = body.iter().position(|&b| b == 0) {
                    let kw = String::from_utf8_lossy(&body[..i]).into_owned();
                    // body[i+1] = compression method, then zlib stream
                    if body.len() > i + 2 {
                        if let Some(txt) = inflate(&body[i + 2..]) {
                            out.insert(kw, txt);
                        }
                    }
                }
            }
            b"iTXt" => {
                // keyword \0 comp_flag comp_method \0 lang \0 translated \0 text
                if let Some(i) = body.iter().position(|&b| b == 0) {
                    let kw = String::from_utf8_lossy(&body[..i]).into_owned();
                    let rest = &body[i + 1..];
                    if rest.len() >= 2 {
                        let comp_flag = rest[0];
                        let mut r = &rest[2..];
                        // skip lang tag and translated keyword (two null-terminated)
                        for _ in 0..2 {
                            if let Some(j) = r.iter().position(|&b| b == 0) {
                                r = &r[j + 1..];
                            }
                        }
                        let txt = if comp_flag == 1 {
                            inflate(r)
                        } else {
                            Some(String::from_utf8_lossy(r).into_owned())
                        };
                        if let Some(t) = txt {
                            out.insert(kw, t);
                        }
                    }
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        pos = body_end + 4; // skip CRC
    }
    out
}

fn inflate(data: &[u8]) -> Option<String> {
    let mut d = ZlibDecoder::new(data);
    let mut s = String::new();
    d.read_to_string(&mut s).ok().map(|_| s)
}

/// True if the PNG already carries a ComfyUI `workflow` (UI) or `prompt` (API)
/// metadata chunk — i.e. dropping it into ComfyUI would load a graph.
pub fn has_embedded_workflow(data: &[u8]) -> bool {
    let c = text_chunks(data);
    c.contains_key("workflow") || c.contains_key("prompt")
}

/// PNG CRC-32 (IEEE 802.3, reflected) over a chunk's type+data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Insert a `tEXt` chunk (`keyword\0text`) immediately before the first IDAT, so
/// ComfyUI reads it as embedded metadata. Returns the new PNG bytes, or None if
/// `data` is not a PNG.
pub fn insert_text_chunk(data: &[u8], keyword: &str, text: &str) -> Option<Vec<u8>> {
    if data.len() < 8 || &data[..8] != PNG_SIG {
        return None;
    }
    let mut body = Vec::with_capacity(keyword.len() + 1 + text.len());
    body.extend_from_slice(keyword.as_bytes());
    body.push(0);
    body.extend_from_slice(text.as_bytes());

    let mut typed = Vec::with_capacity(4 + body.len());
    typed.extend_from_slice(b"tEXt");
    typed.extend_from_slice(&body);

    let mut chunk = Vec::with_capacity(typed.len() + 8);
    chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
    chunk.extend_from_slice(&typed);
    chunk.extend_from_slice(&crc32(&typed).to_be_bytes());

    // walk chunks to find the first IDAT (or IEND) and splice in before it
    let mut pos = 8usize;
    let mut at = None;
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let ctype = &data[pos + 4..pos + 8];
        if ctype == b"IDAT" || ctype == b"IEND" {
            at = Some(pos);
            break;
        }
        pos = pos.checked_add(12)?.checked_add(len)?;
    }
    let at = at?;
    let mut out = Vec::with_capacity(data.len() + chunk.len());
    out.extend_from_slice(&data[..at]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&data[at..]);
    Some(out)
}

/// (comfy_workflow_json_text, a1111_parameters_text)
pub fn split_meta(chunks: &HashMap<String, String>) -> (Option<String>, Option<String>) {
    let mut workflow = None;
    for key in ["workflow", "prompt"] {
        if let Some(v) = chunks.get(key) {
            if serde_json::from_str::<serde_json::Value>(v).is_ok() {
                workflow = Some(v.clone());
                break;
            }
        }
    }
    (workflow, chunks.get("parameters").cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PNG = signature + IEND. insert_text_chunk should splice a tEXt
    /// before IEND, and text_chunks must read it back (validates the CRC too).
    #[test]
    fn insert_and_read_text_chunk_roundtrips() {
        let mut png = PNG_SIG.to_vec();
        png.extend_from_slice(&[0, 0, 0, 0]); // IEND length
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]); // IEND CRC

        assert!(!has_embedded_workflow(&png));
        let body = "{\"nodes\":[],\"links\":[]}";
        let out = insert_text_chunk(&png, "workflow", body).unwrap();

        let chunks = text_chunks(&out);
        assert_eq!(chunks.get("workflow").map(String::as_str), Some(body));
        assert!(has_embedded_workflow(&out));
        // IEND still last, total grew by the new chunk
        assert_eq!(&out[out.len() - 8..out.len() - 4], b"IEND");
    }

    #[test]
    fn insert_rejects_non_png() {
        assert!(insert_text_chunk(b"not a png", "workflow", "{}").is_none());
    }
}
