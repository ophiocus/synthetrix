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
