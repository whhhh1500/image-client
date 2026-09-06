-- Isolated acceptance fixture only. Valid under the full migrated schema and
-- foreign_keys=ON; includes real chapter/source ownership for pending jobs.
INSERT INTO novel_works(id,project_id,title,status,created_at,updated_at)
VALUES('legacy-test-work','legacy-test-project','旧漫画兼容验收','active',1,1);
INSERT INTO novel_chapters(id,novel_work_id,sequence_no,chapter_no,current_revision_id,created_at,updated_at)
VALUES('legacy-test-chapter','legacy-test-work',1,1,'legacy-test-source',1,1),('legacy-test-finished-chapter','legacy-test-work',2,2,'legacy-test-finished-source',1,1);
INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at)
VALUES('legacy-test-source','legacy-test-chapter',1,'旧小说原著正文，必须保留。','legacy-test-hash','paste',1),('legacy-test-finished-source','legacy-test-finished-chapter',1,'旧完成章节正文','legacy-test-finished-hash','paste',1);
INSERT INTO novel_analysis_lineages(id,novel_work_id,name,status,created_at,updated_at)
VALUES('legacy-test-lineage','legacy-test-work','旧分析主线','active',1,1);
INSERT INTO source_analysis_runs(id,novel_chapter_revision_id,novel_analysis_lineage_id,frozen_input_fingerprint,provider_id,model_id,status,prompt_version,schema_version,idempotency_key,created_at,updated_at)
VALUES('legacy-test-analysis','legacy-test-source','legacy-test-lineage','legacy-fingerprint','local-mock','local-model','running','legacy-prompt','legacy-schema','legacy-analysis-key',1,1);
INSERT INTO source_analysis_run_attempts(id,source_analysis_run_id,attempt_no,status,lease_owner,lease_expires_at,created_at)
VALUES('legacy-test-attempt','legacy-test-analysis',1,'running','old-session',9999999999999,1);
INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,created_at,updated_at)
VALUES('legacy-test-job','legacy-test-project','legacy-test-work','legacy-test-chapter','legacy-test-source','legacy-job-hash','legacy-job-key','queued','saving_source',0,8,1,1);
INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,safe_user_message,created_at,updated_at,finished_at)
VALUES('legacy-test-finished-job','legacy-test-project','legacy-test-work','legacy-test-finished-chapter','legacy-test-finished-source','legacy-finished-hash','legacy-finished-key','succeeded','succeeded',8,8,'原完成记录',1,1,1);
INSERT INTO comic_visual_batches(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,output_target,authorization_json,authorization_fingerprint,request_hash,idempotency_key,status,created_at,updated_at)
VALUES('legacy-test-batch','legacy-test-project','legacy-test-work','legacy-test-chapter','legacy-test-source','legacy-test-job','comic_pages','{"model":"local-model","providerId":"gateway","size":"1024x1536","target":"comic_pages"}','sha256:2d318375bf94ebb3d49b566e661b31cdf4debe6fc8d3d6f1d49490459407f1fd','legacy-batch-hash','legacy-batch-key','authorized',1,1);
