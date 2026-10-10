-- v20: a production request may freeze a chapter-scoped comic page intent.
-- It is nullable so legacy jobs keep their historic behavior unchanged.

ALTER TABLE novel_production_jobs
ADD COLUMN comic_plan_intent_json TEXT
  CHECK(comic_plan_intent_json IS NULL OR (json_valid(comic_plan_intent_json) AND json_type(comic_plan_intent_json)='object'));

CREATE TRIGGER novel_production_job_comic_plan_intent_immutable
BEFORE UPDATE OF comic_plan_intent_json ON novel_production_jobs
BEGIN
  SELECT RAISE(ABORT, 'production job comic plan intent is immutable');
END;
