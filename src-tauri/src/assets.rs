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
