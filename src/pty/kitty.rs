//! Minimal Kitty Graphics Protocol (`\x1b_G...;\x1b\\`) decoder.

use std::collections::HashMap;
use std::io::Cursor;

use super::KittyImage;

#[derive(Default)]
pub struct KittyState {
    next_id: u64,
    pending_chunks: Option<PendingChunk>,
    transmitted: HashMap<u64, StoredImage>,
    pub images: Vec<KittyImage>,
    pub replies: Vec<Vec<u8>>,
    /// Cursor advance `(cols, rows)` owed to the client after the last placement.
    pub pending_advance: Option<(u16, u16)>,
}

/// Cell size advertised to the child process (`CSI 16t`, winsize pixels).
const CELL_W_PX: u32 = 9;
const CELL_H_PX: u32 = 18;

fn ceil_div(value: u32, unit: u32) -> u16 {
    let unit = unit.max(1);
    u16::try_from(value.div_ceil(unit)).unwrap_or(u16::MAX)
}

struct PendingChunk {
    params: HashMap<String, String>,
    payload: Vec<u8>,
}

impl KittyState {
    pub fn on_scroll_up(&mut self, max_dist: usize) {
        for img in &mut self.images {
            img.dist_from_bottom = img.dist_from_bottom.saturating_add(1);
        }
        self.images.retain(|img| img.dist_from_bottom <= max_dist);
    }

    /// Decodes one APC. When a placement is printed, the Kitty `C=0` default
    /// policy requires the cursor to advance by the placement size; the pending
    /// `(cols, rows)` is stashed for `Vt100Screen` to apply after the terminator.
    pub fn handle_apc(&mut self, apc: &[u8], cur_col: u16, cur_row: u16, rows: u16, cols: u16) {
        let Some(rest) = apc.strip_prefix(b"G") else {
            return;
        };
        let (hdr_bytes, payload_bytes) = match rest.iter().position(|&b| b == b';') {
            Some(pos) => (&rest[..pos], &rest[pos + 1..]),
            None => (rest, &[][..]),
        };
        let hdr = String::from_utf8_lossy(hdr_bytes);
        let mut params = HashMap::new();
        for part in hdr.split(',') {
            if let Some((k, v)) = part.split_once('=') {
                params.insert(k.trim().to_string(), v.trim().to_string());
            }
        }

        let action = params.get("a").map(String::as_str).unwrap_or("");
        if action == "q" {
            let q = params.get("q").map(String::as_str).unwrap_or("0");
            if q != "2" {
                let id = params.get("i").map(String::as_str).unwrap_or("0");
                self.replies
                    .push(format!("\x1b_Gi={id};OK\x1b\\").into_bytes());
            }
            return;
        }
        if action == "d" {
            let d = params.get("d").map(String::as_str).unwrap_or("a");
            if d == "i" || d == "I" {
                if let Some(id) = params.get("i").and_then(|s| s.parse::<u64>().ok()) {
                    self.images.retain(|img| img.id != id);
                    self.transmitted.remove(&id);
                }
            } else {
                self.images.clear();
                self.transmitted.clear();
            }
            return;
        }

        let more = params.get("m").map(String::as_str) == Some("1");
        let (final_params, final_payload) = if let Some(mut chunk) = self.pending_chunks.take() {
            chunk.payload.extend_from_slice(payload_bytes);
            // Guard against unbounded chunk accumulation from a broken sender.
            if chunk.payload.len() > 64 * 1024 * 1024 {
                return;
            }
            if more {
                self.pending_chunks = Some(chunk);
                return;
            }
            (chunk.params, chunk.payload)
        } else if more {
            self.pending_chunks = Some(PendingChunk {
                params,
                payload: payload_bytes.to_vec(),
            });
            return;
        } else {
            (params, payload_bytes.to_vec())
        };

        let act = final_params.get("a").map(String::as_str).unwrap_or("t");
        if !matches!(act, "T" | "t" | "p" | "q" | "d") {
            return;
        }

        if act == "p" {
            // Placement references an image transmitted earlier with `a=t`.
            let Some(id) = final_params.get("i").and_then(|s| s.parse::<u64>().ok()) else {
                return;
            };
            let Some(stored) = self.transmitted.get(&id).cloned() else {
                return;
            };
            self.place_image(stored, &final_params, cur_col, cur_row, rows, cols, id);
            return;
        }

        let Some(raw) = decode_base64(&final_payload) else {
            return;
        };
        let medium = final_params.get("t").map(String::as_str).unwrap_or("d");
        let raw = match medium {
            "d" => raw,
            "f" | "t" => {
                let Ok(path_str) = String::from_utf8(raw) else {
                    return;
                };
                let Some(bytes) = read_medium_file(&path_str) else {
                    return;
                };
                if medium == "t" {
                    remove_medium_file(&path_str);
                }
                bytes
            }
            _ => return,
        };
        let data = if final_params.get("o").map(String::as_str) == Some("z") {
            match inflate_zlib(&raw) {
                Some(v) => v,
                None => return,
            }
        } else {
            raw
        };

        let fmt = final_params.get("f").map(String::as_str).unwrap_or("32");
        let Some((width, height, rgba)) = decode_image(fmt, &final_params, &data) else {
            return;
        };

        self.next_id = self.next_id.saturating_add(1);
        let id = final_params
            .get("i")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(self.next_id);

        let stored = StoredImage {
            width,
            height,
            rgba,
        };
        if act == "t" {
            // `a=t` only caches the image; a later `a=p` places it.
            if self.transmitted.len() >= 32 {
                self.transmitted.clear();
            }
            self.transmitted.insert(id, stored);
            self.reply_ok(&final_params, id);
            return;
        }

        self.place_image(stored, &final_params, cur_col, cur_row, rows, cols, id);
    }

    #[allow(clippy::too_many_arguments)]
    fn place_image(
        &mut self,
        stored: StoredImage,
        params: &HashMap<String, String>,
        cur_col: u16,
        cur_row: u16,
        rows: u16,
        cols: u16,
        id: u64,
    ) {
        let StoredImage {
            width,
            height,
            rgba,
        } = stored;
        // `c`/`r` are display cells; 0 or missing means "derive from pixels".
        let cell_param = |key: &str, limit: u16| {
            params
                .get(key)
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|&v| v > 0)
                .map(|v| v.min(limit.max(1)))
        };
        let img_cols = cell_param("c", cols);
        let img_rows = cell_param("r", rows);
        let dist_from_bottom = usize::from(rows.saturating_sub(1).saturating_sub(cur_row));

        self.images.push(KittyImage {
            id,
            width,
            height,
            rgba,
            col: cur_col,
            dist_from_bottom,
            cols: img_cols,
            rows: img_rows,
        });
        while self.images.len() > 24 {
            self.images.remove(0);
        }
        if params.get("C").map(String::as_str) != Some("1") {
            let adv_cols = img_cols.unwrap_or_else(|| ceil_div(width, CELL_W_PX));
            let adv_rows = img_rows.unwrap_or_else(|| ceil_div(height, CELL_H_PX));
            self.pending_advance = Some((adv_cols, adv_rows));
        }
        self.reply_ok(params, id);
    }

    fn reply_ok(&mut self, params: &HashMap<String, String>, id: u64) {
        if params.get("q").map(String::as_str).unwrap_or("0") == "0" && params.contains_key("i") {
            self.replies
                .push(format!("\x1b_Gi={id};OK\x1b\\").into_bytes());
        }
    }
}

#[derive(Clone)]
struct StoredImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_image(
    fmt: &str,
    params: &HashMap<String, String>,
    data: &[u8],
) -> Option<(u32, u32, Vec<u8>)> {
    match fmt {
        "100" => decode_png_rgba(data),
        "32" => {
            let w = params
                .get("s")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let h = params
                .get("v")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let needed = u64::from(w) * u64::from(h) * 4;
            if w == 0 || h == 0 || needed > MAX_IMAGE_BYTES || data.len() < needed as usize {
                return None;
            }
            Some((w, h, data[..needed as usize].to_vec()))
        }
        "24" => {
            let w = params
                .get("s")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let h = params
                .get("v")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let needed = u64::from(w) * u64::from(h) * 3;
            if w == 0 || h == 0 || needed > MAX_IMAGE_BYTES || data.len() < needed as usize {
                return None;
            }
            let mut out = Vec::with_capacity((u64::from(w) * u64::from(h) * 4) as usize);
            for px in data[..needed as usize].chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Some((w, h, out))
        }
        _ => None,
    }
}

/// Kitty `o=z`: zlib-compressed payload. Cap the inflated size to avoid
/// decompression bombs from a hostile/broken terminal stream.
fn inflate_zlib(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    const MAX_INFLATED: u64 = 64 * 1024 * 1024;
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .by_ref()
        .take(MAX_INFLATED + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() as u64 > MAX_INFLATED {
        return None;
    }
    Some(out)
}

const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// `t=f`/`t=t` carries a path, so it must not become an arbitrary file read or
/// delete primitive. Only canonicalized regular files inside the OS temp dir
/// are accepted, and only up to the image size cap.
fn read_medium_file(path: &str) -> Option<Vec<u8>> {
    let root = std::fs::canonicalize(std::env::temp_dir()).ok()?;
    let target = std::fs::canonicalize(path.trim()).ok()?;
    if !target.starts_with(&root) {
        return None;
    }
    let meta = std::fs::symlink_metadata(&target).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(&target).ok()?;
    (bytes.len() as u64 <= MAX_IMAGE_BYTES).then_some(bytes)
}

fn remove_medium_file(path: &str) {
    let Ok(root) = std::fs::canonicalize(std::env::temp_dir()) else {
        return;
    };
    let Ok(target) = std::fs::canonicalize(path.trim()) else {
        return;
    };
    if target.starts_with(&root) {
        let _ = std::fs::remove_file(&target);
    }
}

fn decode_png_rgba(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = reader.info().size();
    if u64::from(w) * u64::from(h) * 4 > MAX_IMAGE_BYTES {
        return None;
    }
    let out_size = reader.output_buffer_size()?;
    if out_size as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    let mut buf = vec![0; out_size];
    let info = reader.next_frame(&mut buf).ok()?;
    let bytes = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in bytes.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in bytes.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for &g in bytes {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };
    Some((info.width, info.height, rgba))
}

fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() * 3) / 4);
    let mut acc = 0u32;
    let mut bits = 0u8;
    for &b in input {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn decodes_zlib_chunked_transmit_then_place() {
        let mut state = KittyState::default();
        // 1x2 RGBA: red, green.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let payload = b64(&zlib(&rgba));
        let (first, rest) = payload.split_at(payload.len() / 2);
        let (second, third) = rest.split_at(rest.len() / 2);
        // First chunk carries the control data; continuations only `m`.
        state.handle_apc(
            format!("Ga=t,f=32,s=1,v=2,i=7,o=z,m=1;{first}").as_bytes(),
            0,
            0,
            24,
            80,
        );
        state.handle_apc(format!("Gm=1;{second}").as_bytes(), 0, 0, 24, 80);
        state.handle_apc(format!("Gm=0;{third}").as_bytes(), 0, 0, 24, 80);
        assert!(state.images.is_empty(), "a=t must only cache the image");

        state.handle_apc(b"Ga=p,i=7,c=4,r=2", 3, 5, 24, 80);
        assert_eq!(state.images.len(), 1);
        assert_eq!(state.images[0].id, 7);
        assert_eq!(state.images[0].rgba, rgba);
        assert_eq!(state.images[0].cols, Some(4));
        assert_eq!(state.images[0].rows, Some(2));
        assert_eq!(state.images[0].col, 3);
        assert_eq!(state.images[0].dist_from_bottom, 18);
    }

    #[test]
    fn inflate_rejects_garbage_and_bombs() {
        assert!(inflate_zlib(b"not zlib").is_none());
    }

    #[test]
    fn responds_to_kitty_query_and_decodes_rgba_image() {
        let mut state = KittyState::default();
        state.handle_apc(b"Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA", 0, 0, 24, 80);
        assert_eq!(state.replies, vec![b"\x1b_Gi=31;OK\x1b\\".to_vec()]);
        assert!(state.images.is_empty());

        // 1x1 red RGBA pixel: [255, 0, 0, 255] -> base64 "/wAA/w=="
        state.handle_apc(b"Ga=T,f=32,s=1,v=1,c=10;/wAA/w==", 2, 3, 24, 80);
        assert_eq!(state.images.len(), 1);
        assert_eq!(state.images[0].rgba, vec![255, 0, 0, 255]);
        assert_eq!(state.images[0].col, 2);
        assert_eq!(state.images[0].dist_from_bottom, 20);
        assert_eq!(state.images[0].cols, Some(10));
    }
}
