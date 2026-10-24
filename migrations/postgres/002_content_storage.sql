CREATE TABLE content_store (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    identity TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK (namespace ~ '^[0-9a-f]{32}$')
);
ALTER TABLE versions ALTER COLUMN content DROP NOT NULL;
ALTER TABLE versions ADD COLUMN object_key TEXT;
ALTER TABLE versions ADD CHECK (
    (content IS NOT NULL AND object_key IS NULL AND octet_length(content) = size_bytes)
    OR (content IS NULL AND object_key IS NOT NULL AND length(object_key) > 0)
);
