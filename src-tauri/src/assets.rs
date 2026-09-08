use std::path::Path;

use uuid::Uuid;

use crate::model::AssetRef;

/// Detect supported image formats without guessing for unknown/corrupt data.
pub fn detect_format_checked(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        Some("jpg")
    } else if bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && bytes[8] == b'W'
        && bytes[9] == b'E'
        && bytes[10] == b'B'
        && bytes[11] == b'P'
    {
        Some("webp")
    } else if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        Some("gif")
    } else if bytes.len() >= 2 && bytes[0] == b'B' && bytes[1] == b'M' {
        Some("bmp")
    } else {
        None
    }
}

pub fn is_valid_image_bytes(bytes: &[u8]) -> bool {
    detect_format_checked(bytes).is_some()
}

/// Strict decode bounds. A 50 MiB PNG can declare billions of pixels, and an
/// allocation failure inside a decoder aborts the process (no unwinding), so
/// every user-controlled decode goes through these limits.
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
pub const MAX_IMAGE_ALLOC_BYTES: u64 = 1024 * 1024 * 1024;

pub fn decode_image_checked(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("识别图片格式失败: {error}"))?;
    // `image::Limits` is non-exhaustive, so start from Default and tighten it.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("解码图片失败: {error}"))
}

/// Decode-and-discard, for content validation paths that never use the pixels.
pub fn validate_image_checked(bytes: &[u8]) -> Result<(), String> {
    decode_image_checked(bytes).map(|_| ())
}

pub fn is_valid_mp4_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

pub fn is_valid_video_bytes(bytes: &[u8], extension: &str) -> bool {
    match extension {
        "mp4" | "mov" => is_valid_mp4_bytes(bytes),
        "webm" => bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => false,
    }
}

/// SHA-256 (lowercase hex) of a file, cached while `(path, mtime, len)` is
/// unchanged. Re-hashing hundreds of megabytes on every "open export" click was
/// pure waste; the cache keeps the external-modification check honest because a
/// changed file always changes its mtime or length.
pub fn cached_file_sha256(path: &str, max_bytes: u64) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::UNIX_EPOCH;

    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    static CACHE: OnceLock<Mutex<HashMap<String, (u128, u64, String)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some((cached_modified, cached_len, digest)) = guard.get(path) {
            if *cached_modified == modified && *cached_len == meta.len() {
                return Some(digest.clone());
            }
        }
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_string(), (modified, meta.len(), digest.clone()));
    }
    Some(digest)
}

fn ext_for_kind(kind: &str, format: &str) -> String {
    if kind == "video" {
        return format.to_string(); // e.g. "mp4"
    }
    format.to_string()
}

/// Write bytes into the assets dir and return an AssetRef.
pub fn save_bytes(dir: &Path, kind: &str, bytes: &[u8], format: &str) -> Result<AssetRef, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建资产目录失败: {e}"))?;
    let id = Uuid::new_v4().to_string();
    let ext = ext_for_kind(kind, format);
    let filename = format!("{id}.{ext}");
    let path = dir.join(&filename);
    std::fs::write(&path, bytes).map_err(|e| format!("写入资产失败: {e}"))?;
    Ok(AssetRef {
        id,
        kind: kind.to_string(),
        path: path.display().to_string(),
        width: None,
        height: None,
        duration_s: None,
        format: Some(ext),
    })
}

#[cfg(test)]
mod tests {
    use super::detect_format_checked;

    #[test]
    fn rejects_unknown_image_payloads() {
        assert_eq!(detect_format_checked(b"<html>error</html>"), None);
        assert_eq!(
            detect_format_checked(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some("png")
        );
        assert_eq!(detect_format_checked(b"BM\x00\x00"), Some("bmp"));
    }

    #[test]
    fn recognizes_mp4_by_ftyp_box() {
        assert!(super::is_valid_mp4_bytes(b"\x00\x00\x00\x18ftypisom"));
        assert!(!super::is_valid_mp4_bytes(b"not-a-video"));
    }

    #[test]
    fn recognizes_supported_video_containers() {
        assert!(super::is_valid_video_bytes(
            b"\x00\x00\x00\x18ftypisom",
            "mp4"
        ));
        assert!(super::is_valid_video_bytes(b"\x1A\x45\xDF\xA3webm", "webm"));
        assert!(!super::is_valid_video_bytes(b"not-video", "mov"));
    }
}
