"""Extract embedded generation metadata from PNG originals (stdlib only).

ComfyUI stores the full graph in tEXt chunks keyed 'workflow' (UI graph) and
'prompt' (API graph). A1111/Forge store a single 'parameters' chunk. JPEG/WebP
originals re-encoded by CivitAI carry nothing recoverable.
"""
from __future__ import annotations

import json
import struct
import zlib

PNG_SIG = b"\x89PNG\r\n\x1a\n"


def png_text_chunks(data: bytes) -> dict[str, str]:
    """Return {keyword: text} from tEXt / zTXt / iTXt chunks of a PNG."""
    out: dict[str, str] = {}
    if not data.startswith(PNG_SIG):
        return out
    pos = len(PNG_SIG)
    n = len(data)
    while pos + 8 <= n:
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        ctype = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length  # length + type + data + crc
        try:
            if ctype == b"tEXt":
                kw, _, txt = body.partition(b"\x00")
                out[kw.decode("latin-1")] = txt.decode("latin-1", "replace")
            elif ctype == b"zTXt":
                kw, _, rest = body.partition(b"\x00")
                # rest[0] = compression method, then zlib stream
                out[kw.decode("latin-1")] = zlib.decompress(
                    rest[1:]).decode("latin-1", "replace")
            elif ctype == b"iTXt":
                kw, _, rest = body.partition(b"\x00")
                comp_flag = rest[0]
                # skip comp_method, lang, translated keyword (3 null-separated)
                rest = rest[2:]
                for _ in range(2):
                    _, _, rest = rest.partition(b"\x00")
                out[kw.decode("latin-1")] = (
                    zlib.decompress(rest) if comp_flag else rest
                ).decode("utf-8", "replace")
            elif ctype == b"IDAT" or ctype == b"IEND":
                break  # pixel data reached; no more text chunks of interest
        except Exception:
            continue
    return out


def split_meta(chunks: dict[str, str]) -> tuple[dict | None, str | None]:
    """Return (comfy_workflow_dict, a1111_parameters_str) from chunks."""
    workflow = None
    for key in ("workflow", "prompt"):
        if key in chunks:
            try:
                workflow = json.loads(chunks[key])
                break
            except json.JSONDecodeError:
                pass
    params = chunks.get("parameters")
    return workflow, params
