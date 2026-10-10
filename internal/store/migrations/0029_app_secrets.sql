-- The media-hosting token must not be readable by the renderer, which can issue
-- arbitrary SELECTs against the whitelisted `settings` table. Move it to a table
-- that is deliberately absent from db::ALLOWED_TABLES.
CREATE TABLE IF NOT EXISTS app_secrets (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

INSERT INTO app_secrets(key, value)
SELECT key, value FROM settings WHERE key = 'media_hosting.v1'
ON CONFLICT(key) DO NOTHING;

DELETE FROM settings WHERE key = 'media_hosting.v1';
