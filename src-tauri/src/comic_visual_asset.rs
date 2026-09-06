use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    db::{self, DbState},
    model::AssetRef,
};

const ASSET_UNAVAILABLE: &str = "VISUAL_PAGE_ASSET_UNAVAILABLE";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualPageAssetGetInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub run_id: String,
}

#[tauri::command]
pub fn comic_visual_page_asset_get(
    state: tauri::State<'_, DbState>,
    input: ComicVisualPageAssetGetInput,
) -> Result<AssetRef, String> {
    let output_dir = crate::comic_visual_render::run_output_path(&input.run_id);
    db::with_connection(&state, |conn| get_inner(conn, &input, &output_dir))
}

/// Reads only an asset that is tied to one immutable visual-page run.  The
/// caller supplies the expected run directory so tests can validate the same
/// filesystem boundary without depending on the user's asset directory.
pub(crate) fn get_inner(
    conn: &Connection,
    input: &ComicVisualPageAssetGetInput,
    output_dir: &Path,
) -> Result<AssetRef, String> {
    let row = conn
        .query_row(
            "SELECT asset.id, asset.kind, asset.path, asset.width, asset.height, \
                    asset.duration_s, asset.format, asset.metadata, \
                    manifest.project_id, manifest.novel_work_id, \
                    manifest.production_chapter_id, run.manifest_id, \
                    run.production_page_id, run.generation_attempt_id, \
                    run.manifest_fingerprint \
             FROM comic_visual_page_runs run \
             JOIN comic_visual_manifests manifest ON manifest.id=run.manifest_id \
             JOIN comic_production_pages page ON page.id=run.production_page_id \
             JOIN assets asset ON asset.id=run.asset_id \
             WHERE run.id=? AND run.project_id=? AND run.novel_work_id=? \
               AND run.status='candidate_ready' \
               AND manifest.project_id=? AND manifest.novel_work_id=? \
               AND run.manifest_fingerprint=manifest.manifest_fingerprint \
               AND page.comic_production_chapter_id=manifest.production_chapter_id",
            params![
                &input.run_id,
                &input.project_id,
                &input.novel_work_id,
                &input.project_id,
                &input.novel_work_id,
            ],
            |row| {
                Ok(AssetCandidateRow {
                    asset: AssetRef {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        path: row.get(2)?,
                        width: read_dimension(row.get(3)?)?,
                        height: read_dimension(row.get(4)?)?,
                        duration_s: row.get(5)?,
                        format: row.get(6)?,
                    },
                    metadata: row.get(7)?,
                    project_id: row.get(8)?,
                    novel_work_id: row.get(9)?,
                    production_chapter_id: row.get(10)?,
                    manifest_id: row.get(11)?,
                    production_page_id: row.get(12)?,
                    generation_attempt_id: row.get(13)?,
                    manifest_fingerprint: row.get(14)?,
                })
            },
        )
        .optional()
        .map_err(|_| ASSET_UNAVAILABLE.to_string())?
        .ok_or_else(|| ASSET_UNAVAILABLE.to_string())?;

    validate_lineage(&row, input)?;
    crate::comic_visual_render::validate_candidate_asset(&row.asset, output_dir)
        .map_err(|_| ASSET_UNAVAILABLE.to_string())?;
    Ok(row.asset)
}

struct AssetCandidateRow {
    asset: AssetRef,
    metadata: String,
    project_id: String,
    novel_work_id: String,
    production_chapter_id: String,
    manifest_id: String,
    production_page_id: String,
    generation_attempt_id: String,
    manifest_fingerprint: String,
}

fn read_dimension(value: Option<i64>) -> rusqlite::Result<Option<u32>> {
    value
        .map(|dimension| {
            u32::try_from(dimension)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, dimension))
        })
        .transpose()
}

fn validate_lineage(
    row: &AssetCandidateRow,
    input: &ComicVisualPageAssetGetInput,
) -> Result<(), String> {
    let metadata: Value =
        serde_json::from_str(&row.metadata).map_err(|_| ASSET_UNAVAILABLE.to_string())?;
    let expected = [
        ("projectId", input.project_id.as_str()),
        ("novelWorkId", input.novel_work_id.as_str()),
        ("productionChapterId", row.production_chapter_id.as_str()),
        ("comicVisualManifestId", row.manifest_id.as_str()),
        ("productionPageId", row.production_page_id.as_str()),
        ("visualRunId", input.run_id.as_str()),
        ("generationAttemptId", row.generation_attempt_id.as_str()),
        ("manifestFingerprint", row.manifest_fingerprint.as_str()),
    ];

    if row.project_id != input.project_id || row.novel_work_id != input.novel_work_id {
        return Err(ASSET_UNAVAILABLE.into());
    }
    if expected
        .iter()
        .any(|(key, expected)| metadata.get(*key).and_then(Value::as_str) != Some(*expected))
    {
        return Err(ASSET_UNAVAILABLE.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::{get_inner, ComicVisualPageAssetGetInput, ASSET_UNAVAILABLE};

    struct FixtureRoot(PathBuf);

    impl FixtureRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("comic-visual-asset-{}", uuid::Uuid::new_v4())))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for FixtureRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_candidate_image(root: &Path) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join("candidate.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        path
    }

    fn fixture_connection(path: &Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE comic_visual_manifests (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, novel_work_id TEXT NOT NULL,
                production_chapter_id TEXT NOT NULL, manifest_fingerprint TEXT NOT NULL
              );
              CREATE TABLE comic_production_pages (
                id TEXT PRIMARY KEY, comic_production_chapter_id TEXT NOT NULL
              );
              CREATE TABLE comic_visual_page_runs (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, novel_work_id TEXT NOT NULL,
                manifest_id TEXT NOT NULL, production_page_id TEXT NOT NULL,
                generation_attempt_id TEXT NOT NULL, manifest_fingerprint TEXT NOT NULL,
                asset_id TEXT, status TEXT NOT NULL
              );
              CREATE TABLE assets (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, path TEXT NOT NULL,
                width INTEGER, height INTEGER, duration_s REAL, format TEXT, metadata TEXT NOT NULL
              );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO comic_visual_manifests
             (id,project_id,novel_work_id,production_chapter_id,manifest_fingerprint)
             VALUES ('manifest','project','work','chapter','fingerprint')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO comic_production_pages (id,comic_production_chapter_id)
             VALUES ('page','chapter')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO comic_visual_page_runs
             (id,project_id,novel_work_id,manifest_id,production_page_id,generation_attempt_id,
              manifest_fingerprint,asset_id,status)
             VALUES ('run','project','work','manifest','page','attempt','fingerprint','asset',
                     'candidate_ready')",
            [],
        )
        .unwrap();
        let metadata = json!({
            "projectId":"project", "novelWorkId":"work", "productionChapterId":"chapter",
            "comicVisualManifestId":"manifest", "productionPageId":"page", "visualRunId":"run",
            "generationAttemptId":"attempt", "manifestFingerprint":"fingerprint"
        });
        conn.execute(
            "INSERT INTO assets (id,kind,path,width,height,duration_s,format,metadata)
             VALUES ('asset','image',?,1,1,NULL,'png',?)",
            params![path.to_string_lossy(), metadata.to_string()],
        )
        .unwrap();
        conn
    }

    fn input() -> ComicVisualPageAssetGetInput {
        ComicVisualPageAssetGetInput {
            project_id: "project".into(),
            novel_work_id: "work".into(),
            run_id: "run".into(),
        }
    }

    #[test]
    fn reads_a_candidate_ready_asset_with_complete_lineage() {
        let root = FixtureRoot::new();
        let path = write_candidate_image(root.path());
        let conn = fixture_connection(&path);

        let asset = get_inner(&conn, &input(), root.path()).unwrap();

        assert_eq!(asset.id, "asset");
        assert_eq!(asset.path, path.to_string_lossy());
    }

    #[test]
    fn rejects_assets_when_the_persisted_lineage_is_incomplete() {
        let root = FixtureRoot::new();
        let path = write_candidate_image(root.path());
        let conn = fixture_connection(&path);
        conn.execute(
            "UPDATE assets SET metadata=json_remove(metadata, '$.generationAttemptId') WHERE id='asset'",
            [],
        )
        .unwrap();

        let error = get_inner(&conn, &input(), root.path()).unwrap_err();

        assert_eq!(error, ASSET_UNAVAILABLE);
    }

    #[test]
    fn rejects_a_candidate_outside_the_requested_project_or_work() {
        let root = FixtureRoot::new();
        let path = write_candidate_image(root.path());
        let conn = fixture_connection(&path);
        let wrong_project = ComicVisualPageAssetGetInput {
            project_id: "other-project".into(),
            ..input()
        };
        let wrong_work = ComicVisualPageAssetGetInput {
            novel_work_id: "other-work".into(),
            ..input()
        };

        assert_eq!(
            get_inner(&conn, &wrong_project, root.path()).unwrap_err(),
            ASSET_UNAVAILABLE
        );
        assert_eq!(
            get_inner(&conn, &wrong_work, root.path()).unwrap_err(),
            ASSET_UNAVAILABLE
        );
    }

    #[test]
    fn rejects_runs_that_are_not_candidate_ready() {
        let root = FixtureRoot::new();
        let path = write_candidate_image(root.path());
        let conn = fixture_connection(&path);
        conn.execute(
            "UPDATE comic_visual_page_runs SET status='failed' WHERE id='run'",
            [],
        )
        .unwrap();

        assert_eq!(
            get_inner(&conn, &input(), root.path()).unwrap_err(),
            ASSET_UNAVAILABLE
        );
    }

    #[test]
    fn rejects_an_asset_that_escapes_the_run_directory_or_is_not_an_image() {
        let fixture = FixtureRoot::new();
        let output_root = fixture.path().join("run");
        let candidate = write_candidate_image(&output_root);
        let escaped = fixture.path().join("escaped.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]))
            .save_with_format(&escaped, image::ImageFormat::Png)
            .unwrap();
        let conn = fixture_connection(&candidate);
        conn.execute(
            "UPDATE assets SET path=? WHERE id='asset'",
            params![escaped.to_string_lossy()],
        )
        .unwrap();

        assert_eq!(
            get_inner(&conn, &input(), &output_root).unwrap_err(),
            ASSET_UNAVAILABLE
        );

        std::fs::write(&candidate, b"not an image").unwrap();
        conn.execute(
            "UPDATE assets SET path=? WHERE id='asset'",
            params![candidate.to_string_lossy()],
        )
        .unwrap();
        assert_eq!(
            get_inner(&conn, &input(), &output_root).unwrap_err(),
            ASSET_UNAVAILABLE
        );
    }
}
