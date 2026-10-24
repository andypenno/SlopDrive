use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub(super) enum DataType {
    Text,
    Integer,
    Bytes,
    Hex(usize),
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct Column {
    pub name: &'static str,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Clone, Copy, Serialize)]
pub(super) enum Constraint {
    PrimaryKey(&'static [&'static str]),
    Unique(&'static [&'static str]),
    ForeignKey {
        columns: &'static [&'static str],
        table: &'static str,
        references: &'static [&'static str],
        deferred: bool,
    },
    Range(&'static str, i64, i64),
    Positive(&'static str),
    Equals(&'static str, i64),
    ContentLocation {
        bytes: &'static str,
        key: &'static str,
        size: &'static str,
    },
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct Table {
    pub name: &'static str,
    pub columns: &'static [Column],
    pub constraints: &'static [Constraint],
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct Migration {
    pub version: i64,
    pub tables: &'static [Table],
}
