CREATE TABLE uploads (
    id TEXT PRIMARY KEY CHECK (id ~ '^[0-9a-f]{32}$'),
    latest_hash TEXT NOT NULL
);
CREATE TABLE versions (
    upload_id TEXT NOT NULL REFERENCES uploads(id),
    content_hash TEXT NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    created_at BIGINT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 1048576),
    version_number BIGINT NOT NULL CHECK (version_number > 0),
    content BYTEA NOT NULL CHECK (octet_length(content) = size_bytes),
    PRIMARY KEY (upload_id, content_hash),
    UNIQUE (upload_id, version_number)
);
ALTER TABLE uploads ADD FOREIGN KEY (id, latest_hash) REFERENCES versions(upload_id, content_hash) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY CHECK (id ~ '^[0-9a-f]{16}$'),
    secret_hash TEXT NOT NULL CHECK (secret_hash ~ '^[0-9a-f]{64}$')
);
