use super::model::{Constraint, DataType, Migration, Table};

#[derive(Clone, Copy)]
pub(super) enum Dialect {
    Sqlite,
    Postgres,
}

fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn columns(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| identifier(name))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Dialect {
    fn byte_length(self, column: &str) -> String {
        match self {
            Self::Sqlite => format!("length(CAST({} AS BLOB))", identifier(column)),
            Self::Postgres => format!("octet_length({})", identifier(column)),
        }
    }

    fn constraint(self, constraint: &Constraint) -> String {
        match constraint {
            Constraint::PrimaryKey(names) => format!("PRIMARY KEY ({})", columns(names)),
            Constraint::Unique(names) => format!("UNIQUE ({})", columns(names)),
            Constraint::ForeignKey {
                columns: names,
                table,
                references,
                deferred,
            } => format!(
                "FOREIGN KEY ({}) REFERENCES {} ({}){}",
                columns(names),
                identifier(table),
                columns(references),
                if *deferred {
                    " DEFERRABLE INITIALLY DEFERRED"
                } else {
                    ""
                }
            ),
            Constraint::Range(name, min, max) => {
                format!("CHECK ({} BETWEEN {min} AND {max})", identifier(name))
            }
            Constraint::Positive(name) => format!("CHECK ({} > 0)", identifier(name)),
            Constraint::Equals(name, value) => format!("CHECK ({} = {value})", identifier(name)),
            Constraint::ContentLocation { bytes, key, size } => format!(
                "CHECK (({b} IS NOT NULL AND {k} IS NULL AND {length} = {s}) OR ({b} IS NULL AND {k} IS NOT NULL AND {key_length} > 0))",
                b = identifier(bytes),
                k = identifier(key),
                s = identifier(size),
                length = self.byte_length(bytes),
                key_length = self.byte_length(key)
            ),
        }
    }

    pub fn table(self, table: &Table, if_absent: bool) -> String {
        let mut entries = Vec::new();
        for column in table.columns {
            let name = identifier(column.name);
            let data_type = match (self, column.data_type) {
                (_, DataType::Text | DataType::Hex(_)) => "TEXT",
                (Self::Sqlite, DataType::Integer) => "INTEGER",
                (Self::Postgres, DataType::Integer) => "BIGINT",
                (Self::Sqlite, DataType::Bytes) => "BLOB",
                (Self::Postgres, DataType::Bytes) => "BYTEA",
            };
            let mut entry = format!(
                "{name} {data_type}{}",
                if column.nullable { "" } else { " NOT NULL" }
            );
            if let DataType::Hex(size) = column.data_type {
                let predicate = match self {
                    // Byte and character lengths together reject embedded NUL and non-ASCII text.
                    Self::Sqlite => format!(
                        "length({name}) = {size} AND {} = {size} AND {name} NOT GLOB '*[^0-9a-f]*'",
                        self.byte_length(column.name)
                    ),
                    Self::Postgres => format!("{name} ~ '^[0-9a-f]{{{size}}}$'"),
                };
                entry.push_str(&format!(" CHECK ({predicate})"));
            }
            entries.push(entry);
        }
        for constraint in table.constraints {
            if matches!(self, Self::Postgres) && matches!(constraint, Constraint::ForeignKey { .. })
            {
                continue;
            }
            entries.push(self.constraint(constraint));
        }
        format!(
            "CREATE TABLE {}{} ({}){};",
            if if_absent { "IF NOT EXISTS " } else { "" },
            identifier(table.name),
            entries.join(", "),
            if matches!(self, Self::Sqlite) {
                " STRICT"
            } else {
                ""
            }
        )
    }

    pub fn migration(self, migration: &Migration) -> String {
        let mut sql = migration
            .tables
            .iter()
            .map(|table| self.table(table, false))
            .collect::<String>();
        if matches!(self, Self::Postgres) {
            for table in migration.tables {
                for constraint in table.constraints {
                    if matches!(constraint, Constraint::ForeignKey { .. }) {
                        sql.push_str(&format!(
                            "ALTER TABLE {} ADD {};",
                            identifier(table.name),
                            self.constraint(constraint)
                        ));
                    }
                }
            }
        }
        sql
    }
}
