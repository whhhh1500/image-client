//! Resolves desktop video-reference identities to bounded in-memory image data URLs.
//!
//! The IPC caller supplies only a project-scoped asset id or a canonical comic
//! catalog URI.  It never supplies a filesystem path or image bytes.

use std::{fs::File, io::Read, path::PathBuf};

use base64::Engine as _;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    comic_markdown,
    db::{self, DbState},
};

pub const MAX_LOCAL_IMAGES: usize = 30;
pub const MAX_LOCAL_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct LocalVideoImageSource {
    pub asset_id: Option<String>,
    pub source_uri: Option<String>,
}

pub fn sources_from_config(config: &Value) -> Result<Vec<LocalVideoImageSource>, String> {
    let Some(value) = config.get("local_images") else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).map_err(|_| "本地图片引用格式无效".to_string())
}

pub fn resolve_local_video_images(
    db: &DbState,
    project_id: &str,
    sources: &[LocalVideoImageSource],
) -> Result<Vec<String>, String> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    if project_id.trim().is_empty() {
        return Err("本地图片引用需要项目标识".into());
    }
    if sources.len() > MAX_LOCAL_IMAGES {
        return Err("本地图片引用最多为 30 张".into());
    }
    let catalog = sources
        .iter()
        .any(|source| {
            source
                .source_uri
                .as_deref()
                .is_some_and(|uri| !uri.trim().is_empty())
        })
        .then(|| {
            db::with_connection(db, |connection| {
                comic_markdown::catalog_list(connection, project_id)
            })
        })
        .transpose()?;

    let mut total_bytes = 0_u64;
    let mut images = Vec::with_capacity(sources.len());
    for source in sources {
        let has_asset = source
            .asset_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty());
        let has_uri = source
            .source_uri
            .as_deref()
            .is_some_and(|uri| !uri.trim().is_empty());
        if has_asset == has_uri {
            return Err("每个本地图片来源必须且只能提供 assetId 或 sourceUri".into());
        }
        let raw_path = if let Some(asset_id) = source
            .asset_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        {
            db::with_connection(db, |connection| {
                let found: Option<(String, String, Option<String>)> = connection
                    .query_row(
                        "SELECT kind,path,metadata FROM assets WHERE id=?",
                        [asset_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(|_| "读取本地图片资产失败".to_string())?;
                let Some((kind, path, metadata)) = found else {
                    return Err("本地图片资产不存在".into());
                };
                let project_matches = metadata
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .and_then(|value| {
                        value
                            .get("projectId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some(project_id);
                if !project_matches {
                    return Err("本地图片资产不属于当前项目".into());
                }
                if kind != "image" {
                    return Err("本地视频引用只能使用图片资产".into());
                }
                Ok(path)
            })?
        } else {
            let source_uri = source.source_uri.as_deref().unwrap().trim();
            let entry = catalog
                .as_ref()
                .expect("catalog is loaded when sourceUri is present")
                .iter()
                .find(|entry| entry.source_uri == source_uri)
                .ok_or("漫画图片来源不存在或不属于当前项目")?;
            if entry.kind != "image" {
                return Err("漫画来源不是图片".into());
            }
            entry.path.clone().ok_or("漫画图片文件不存在")?
        };
        let bytes = read_bounded_image(PathBuf::from(raw_path), &mut total_bytes)?;
        let (format, mime) = verified_format_and_mime(&bytes)?;
        let _ = format;
        images.push(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    Ok(images)
}

fn read_bounded_image(path: PathBuf, total_bytes: &mut u64) -> Result<Vec<u8>, String> {
    let path = path
        .canonicalize()
        .map_err(|_| "本地图片文件不存在或不可访问")?;
    let metadata = std::fs::metadata(&path).map_err(|_| "读取本地图片文件失败")?;
    if !metadata.is_file() {
        return Err("本地图片来源必须是普通文件".into());
    }
    let remaining = MAX_LOCAL_IMAGE_BYTES
        .checked_sub(*total_bytes)
        .ok_or("本地图片总大小不能超过 50 MiB")?;
    if metadata.len() > MAX_LOCAL_IMAGE_BYTES {
        return Err("单张本地图片不能超过 50 MiB".into());
    }
    if metadata.len() > remaining {
        return Err("本地图片总大小不能超过 50 MiB".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "读取本地图片文件失败")?
        .take(remaining.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "读取本地图片文件失败")?;
    let byte_len = u64::try_from(bytes.len()).map_err(|_| "本地图片总大小不能超过 50 MiB")?;
    if byte_len > MAX_LOCAL_IMAGE_BYTES {
        return Err("单张本地图片不能超过 50 MiB".into());
    }
    if byte_len > remaining {
        return Err("本地图片总大小不能超过 50 MiB".into());
    }
    *total_bytes = total_bytes
        .checked_add(byte_len)
        .ok_or("本地图片总大小不能超过 50 MiB")?;
    Ok(bytes)
}

pub fn verified_format_and_mime(bytes: &[u8]) -> Result<(&'static str, &'static str), String> {
    let format =
        crate::assets::detect_format_checked(bytes).ok_or("本地图片格式不受支持或文件已损坏")?;
    let mime = match format {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return Err("本地视频引用仅支持 PNG、JPEG 或 WebP 图片".into()),
    };
    crate::assets::validate_image_checked(bytes)
        .map_err(|_| "本地图片格式不受支持或文件已损坏")?;
    Ok((format, mime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbState;

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn test_db() -> (DbState, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("local-video-images-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        (DbState::open(root.join("test.db")).unwrap(), root)
    }

    #[test]
    fn resolves_a_same_project_asset_to_a_verified_data_url() {
        let (db, root) = test_db();
        let image_path = root.join("reference.png");
        std::fs::write(&image_path, png()).unwrap();
        db::with_connection(&db, |connection| {
            connection.execute(
                "INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('image-a','image',?,1,?)",
                rusqlite::params![image_path.display().to_string(), r#"{"projectId":"project-a"}"#],
            ).map_err(|_| "test insert failed".to_string())?;
            Ok(())
        }).unwrap();
        let resolved = resolve_local_video_images(
            &db,
            "project-a",
            &[LocalVideoImageSource {
                asset_id: Some("image-a".into()),
                source_uri: None,
            }],
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].starts_with("data:image/png;base64,"));
        assert!(!resolved[0].contains(image_path.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_cross_project_non_image_missing_and_oversized_assets() {
        let (db, root) = test_db();
        let image_path = root.join("reference.png");
        std::fs::write(&image_path, png()).unwrap();
        db::with_connection(&db, |connection| {
            connection.execute("INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('other','image',?,1,?)", rusqlite::params![image_path.display().to_string(), r#"{"projectId":"project-b"}"#]).unwrap();
            connection.execute("INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('video','video',?,1,?)", rusqlite::params![image_path.display().to_string(), r#"{"projectId":"project-a"}"#]).unwrap();
            Ok(())
        }).unwrap();
        for id in ["other", "video", "missing"] {
            assert!(resolve_local_video_images(
                &db,
                "project-a",
                &[LocalVideoImageSource {
                    asset_id: Some(id.into()),
                    source_uri: None
                }]
            )
            .is_err());
        }
        let oversized = root.join("oversized.png");
        std::fs::write(&oversized, vec![0_u8; (MAX_LOCAL_IMAGE_BYTES + 1) as usize]).unwrap();
        db::with_connection(&db, |connection| {
            connection.execute("INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('large','image',?,1,?)", rusqlite::params![oversized.display().to_string(), r#"{"projectId":"project-a"}"#]).unwrap();
            Ok(())
        }).unwrap();
        assert!(resolve_local_video_images(
            &db,
            "project-a",
            &[LocalVideoImageSource {
                asset_id: Some("large".into()),
                source_uri: None
            }]
        )
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_only_a_catalogued_canonical_comic_image() {
        let (db, root) = test_db();
        let image_path = root.join("comic.png");
        std::fs::write(&image_path, png()).unwrap();
        db::with_connection(&db, |connection| {
            connection.execute_batch(
                "INSERT INTO novel_works(id,project_id,title,status,created_at,updated_at) VALUES('work','project-a','测试小说','active',1,1);
                 INSERT INTO novel_chapters(id,novel_work_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES('chapter','work',1,1,'第一章','revision',1,1);
                 INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at) VALUES('revision','chapter',1,'正文','hash','paste',1);
                 INSERT INTO comic_md_documents(id,project_id,novel_work_id,chapter_id,kind,page_no,markdown,revision,dependencies,updated_at) VALUES('document','project-a','work','chapter','page_prompt',1,'# 第1页',1,'[]',1);
                 INSERT INTO comic_md_revisions(document_id,revision,markdown,dependencies,created_at) VALUES('document',1,'# 第1页','[]',1);
                 INSERT INTO comic_md_jobs(id,project_id,novel_work_id,chapter_id,kind,status,input_snapshot,created_at) VALUES('job','project-a','work','chapter','images','succeeded','{}',1);",
            ).map_err(|_| "test insert failed".to_string())?;
            connection.execute(
                "INSERT INTO comic_md_images(id,job_id,document_id,document_revision,page_no,path,created_at) VALUES('comic-image','job','document',1,1,?,1)",
                [image_path.display().to_string()],
            ).map_err(|_| "test insert failed".to_string())?;
            Ok(())
        }).unwrap();
        let source_uri = "comic-md://project-a/work/chapter/image/comic-image";
        let resolved = resolve_local_video_images(
            &db,
            "project-a",
            &[LocalVideoImageSource {
                asset_id: None,
                source_uri: Some(source_uri.into()),
            }],
        )
        .unwrap();
        assert!(resolved[0].starts_with("data:image/png;base64,"));
        assert!(resolve_local_video_images(
            &db,
            "project-a",
            &[LocalVideoImageSource {
                asset_id: None,
                source_uri: Some("comic-md://project-b/work/chapter/image/comic-image".into())
            }]
        )
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
