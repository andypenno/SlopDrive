CREATE TABLE uploads (
    id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    latest_hash TEXT NOT NULL,
    FOREIGN KEY (id, latest_hash) REFERENCES versions(upload_id, content_hash) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE versions (
    upload_id TEXT NOT NULL REFERENCES uploads(id),
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
    created_at INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes BETWEEN 1 AND 1048576),
    version_number INTEGER NOT NULL CHECK (version_number > 0),
    content BLOB NOT NULL CHECK (length(content) = size_bytes),
    PRIMARY KEY (upload_id, content_hash),
    UNIQUE (upload_id, version_number)
) STRICT;
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY CHECK (length(id) = 16 AND id NOT GLOB '*[^0-9a-f]*'),
    secret_hash TEXT NOT NULL CHECK (length(secret_hash) = 64 AND secret_hash NOT GLOB '*[^0-9a-f]*')
) STRICT;
