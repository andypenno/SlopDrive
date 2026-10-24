use super::model::{Column, Constraint::*, DataType::*, Migration, Table};

pub(super) const BASELINE: Migration = Migration {
    version: 1,
    tables: &[
        Table {
            name: "uploads",
            columns: &[
                Column {
                    name: "id",
                    data_type: Hex(32),
                    nullable: false,
                },
                Column {
                    name: "latest_hash",
                    data_type: Text,
                    nullable: false,
                },
            ],
            constraints: &[
                PrimaryKey(&["id"]),
                ForeignKey {
                    columns: &["id", "latest_hash"],
                    table: "versions",
                    references: &["upload_id", "content_hash"],
                    deferred: true,
                },
            ],
        },
        Table {
            name: "versions",
            columns: &[
                Column {
                    name: "upload_id",
                    data_type: Text,
                    nullable: false,
                },
                Column {
                    name: "content_hash",
                    data_type: Hex(64),
                    nullable: false,
                },
                Column {
                    name: "created_at",
                    data_type: Integer,
                    nullable: false,
                },
                Column {
                    name: "size_bytes",
                    data_type: Integer,
                    nullable: false,
                },
                Column {
                    name: "version_number",
                    data_type: Integer,
                    nullable: false,
                },
                Column {
                    name: "content",
                    data_type: Bytes,
                    nullable: true,
                },
                Column {
                    name: "object_key",
                    data_type: Text,
                    nullable: true,
                },
            ],
            constraints: &[
                PrimaryKey(&["upload_id", "content_hash"]),
                Unique(&["upload_id", "version_number"]),
                ForeignKey {
                    columns: &["upload_id"],
                    table: "uploads",
                    references: &["id"],
                    deferred: false,
                },
                Range("size_bytes", 1, 1048576),
                Positive("version_number"),
                ContentLocation {
                    bytes: "content",
                    key: "object_key",
                    size: "size_bytes",
                },
            ],
        },
        Table {
            name: "api_keys",
            columns: &[
                Column {
                    name: "id",
                    data_type: Hex(16),
                    nullable: false,
                },
                Column {
                    name: "secret_hash",
                    data_type: Hex(64),
                    nullable: false,
                },
            ],
            constraints: &[PrimaryKey(&["id"])],
        },
        Table {
            name: "content_store",
            columns: &[
                Column {
                    name: "id",
                    data_type: Integer,
                    nullable: false,
                },
                Column {
                    name: "identity",
                    data_type: Text,
                    nullable: false,
                },
                Column {
                    name: "namespace",
                    data_type: Hex(32),
                    nullable: false,
                },
            ],
            constraints: &[PrimaryKey(&["id"]), Equals("id", 1)],
        },
    ],
};
