-- The actual prompt submitted for a rendered page is a historical artifact.
-- Persist it with the image instead of attempting to derive it from mutable
-- render options or work-level visual settings when the catalog is reopened.
ALTER TABLE comic_md_images ADD COLUMN effective_prompt TEXT;
