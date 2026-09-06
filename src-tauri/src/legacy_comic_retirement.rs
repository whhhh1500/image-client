//! Stop retired novel/comic executors without deleting their saved history.
use crate::db::{self, DbState};
use rusqlite::{params, Connection};

const MESSAGE: &str =
    "旧小说漫画流程已停用；已有正文、分析记录和图片保留，请使用 Markdown 漫画工作区。";

pub fn retire_pending(db: &DbState) -> Result<usize, String> {
    db::with_connection(db, retire_pending_on_connection)
}

fn retire_pending_on_connection(conn: &Connection) -> Result<usize, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut changed = 0;
    for statement in [
        "UPDATE novel_production_jobs SET status='error',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,next_action=NULL,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('queued','running','waiting_for_predecessor','blocked_config','blocked_conflict')",
        "UPDATE source_analysis_runs SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,updated_at=?2,completed_at=?2 WHERE status IN ('queued','running')",
        "UPDATE source_analysis_run_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
        "UPDATE adaptation_analysis_runs SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('draft','queued','running')",
        "UPDATE adaptation_analysis_run_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
        "UPDATE novel_artifact_optimization_runs SET status='error',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('queued','running','cancel_requested')",
        "UPDATE novel_artifact_optimization_attempts SET status='cancelled',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running')",
        "UPDATE comic_visual_batches SET status='failed',safe_error_code='LEGACY_COMIC_RETIRED',safe_user_message=?1,lease_owner=NULL,lease_expires_at=NULL,updated_at=?2,finished_at=?2 WHERE status IN ('authorized','waiting_text','preparing','running','blocked_config','needs_reconcile')",
        "UPDATE comic_visual_page_runs SET status='failed',failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','needs_reconcile')",
        "UPDATE comic_page_runs SET status='error',error=?1,failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','submitted','needs_reconcile')",
        "UPDATE comic_panel_runs SET status='error',error=?1,failure_json=json_object('code','LEGACY_COMIC_RETIRED','message',?1),owner_app_session_id=NULL,lease_expires_at=NULL,finished_at=?2 WHERE status IN ('queued','running','submitted','needs_reconcile')",
    ] {
        changed += tx.execute(statement, params![MESSAGE, now]).map_err(|e| format!("停用旧小说漫画任务失败：{e}"))?;
    }
    changed += tx.execute("UPDATE comic_visual_batch_members SET status='failed',updated_at=?1,finished_at=?1 WHERE status IN ('pending','queued','running','blocked_config','needs_reconcile')", [now]).map_err(|e| format!("停用旧漫画页任务失败：{e}"))?;
    // blocked_config visual attempts have attempt_no=0 and cannot change to
    // failed under the v17 CHECK. They have never dispatched; leave that
    // historical config state intact, with no registered executor to revive it.
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retires_valid_pending_history_once_and_preserves_completed_results() {
        let root =
            std::env::temp_dir().join(format!("legacy-comic-retirement-{}", uuid::Uuid::new_v4()));
        let db = DbState::open(root.join("test.db")).unwrap();
        db::with_connection(&db, |c| {
            c.execute_batch(include_str!("../fixtures/legacy-comic-retirement.sql"))
                .unwrap();
            let invalid: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!invalid);
            Ok(())
        })
        .unwrap();
        assert_eq!(retire_pending(&db).unwrap(), 4);
        assert_eq!(retire_pending(&db).unwrap(), 0);
        db::with_connection(&db, |c| {
            for (table,key,expected) in [("novel_production_jobs","legacy-test-job","error"),("comic_visual_batches","legacy-test-batch","failed"),("source_analysis_runs","legacy-test-analysis","cancelled"),("source_analysis_run_attempts","legacy-test-attempt","cancelled")] {
                let (status,code):(String,String)=c.query_row(&format!("SELECT status,safe_error_code FROM {table} WHERE id=?"),[key],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                assert_eq!(status,expected);assert_eq!(code,"LEGACY_COMIC_RETIRED");
            }
            let preserved:(String,String)=c.query_row("SELECT status,safe_user_message FROM novel_production_jobs WHERE id='legacy-test-finished-job'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!(preserved,("succeeded".into(),"原完成记录".into()));
            let raw:String=c.query_row("SELECT content FROM novel_chapter_revisions WHERE id='legacy-test-source'",[],|r|r.get(0)).unwrap();assert_eq!(raw,"旧小说原著正文，必须保留。");
            let count:i64=c.query_row("SELECT count(*) FROM novel_production_jobs",[],|r|r.get(0)).unwrap();assert_eq!(count,2);
            Ok(())
        }).unwrap();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retirement_rolls_back_as_one_transaction() {
        let root =
            std::env::temp_dir().join(format!("legacy-comic-retirement-{}", uuid::Uuid::new_v4()));
        let db = DbState::open(root.join("test.db")).unwrap();
        db::with_connection(&db,|c|{c.execute_batch(include_str!("../fixtures/legacy-comic-retirement.sql")).unwrap();c.execute_batch("CREATE TRIGGER reject_retirement BEFORE UPDATE ON comic_visual_batches BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();Ok(())}).unwrap();
        assert!(retire_pending(&db).is_err());
        db::with_connection(&db, |c| {
            let status: String = c
                .query_row(
                    "SELECT status FROM novel_production_jobs WHERE id='legacy-test-job'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(status, "queued");
            Ok(())
        })
        .unwrap();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
