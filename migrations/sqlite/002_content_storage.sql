CREATE TABLE content_store (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    identity TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK (length(namespace) = 32 AND namespace NOT GLOB '*[^0-9a-f]*')
) STRICT;
CREATE TABLE uploads_new (
    id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    latest_hash TEXT NOT NULL,
    FOREIGN KEY (id, latest_hash) REFERENCES versions_new(upload_id, content_hash) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE versions_new (
    upload_id TEXT NOT NULL REFERENCES uploads_new(id),
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
    created_at INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes BETWEEN 1 AND 1048576),
    version_number INTEGER NOT NULL CHECK (version_number > 0),
    content BLOB,
    object_key TEXT,
    CHECK ((content IS NOT NULL AND object_key IS NULL AND length(content) = size_bytes)
        OR (content IS NULL AND object_key IS NOT NULL AND length(object_key) > 0)),
    PRIMARY KEY (upload_id, content_hash),
    UNIQUE (upload_id, version_number)
) STRICT;
INSERT INTO uploads_new SELECT * FROM uploads;
INSERT INTO versions_new SELECT upload_id, content_hash, created_at, size_bytes, version_number, content, NULL FROM versions;
DROP TABLE versions;
DROP TABLE uploads;
ALTER TABLE uploads_new RENAME TO uploads;
ALTER TABLE versions_new RENAME TO versions;
