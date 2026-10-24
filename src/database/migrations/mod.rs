mod model;
mod render;
mod v001;

use crate::{helpers::hash, storage::StoreError};
use model::{Column, Constraint, DataType, Migration, Table};
use render::Dialect;

const MIGRATIONS: &[Migration] = &[v001::BASELINE];
const LEDGER: Table = Table {
    name: "schema_migrations",
    columns: &[
        Column {
            name: "version",
            data_type: DataType::Integer,
            nullable: false,
        },
        Column {
            name: "checksum",
            data_type: DataType::Text,
            nullable: false,
        },
    ],
    constraints: &[Constraint::PrimaryKey(&["version"])],
};

fn checksum(migration: &Migration) -> String {
    // Ordered structs/slices only. Encoding and enum names are part of the persisted format.
    let canonical = serde_json::to_vec(&("slopbox-schema-v1", LEDGER, migration))
        .expect("static schema serialization cannot fail");
    hash(&canonical)
}

fn schema_error(error: sqlx::Error) -> StoreError {
    match &error {
        sqlx::Error::Database(database)
            if matches!(database.code().as_deref(), Some("1" | "42P07" | "42703")) => {}
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::ColumnNotFound(_) => {}
        _ => return error.into(),
    }
    StoreError::Configuration(
        "incompatible database schema; recreate pre-1.x development databases manually",
    )
}

macro_rules! runner {
    ($name:ident, $db:ty, $dialect:expr) => {
        async fn $name(
            tx: &mut sqlx::Transaction<'_, $db>,
            migrations: &[Migration],
        ) -> Result<bool, StoreError> {
            if migrations.iter().enumerate().any(|(i, m)| m.version != i as i64 + 1) {
                return Err(StoreError::Configuration("invalid migration registry"));
            }
            sqlx::raw_sql(&$dialect.table(&LEDGER, true)).execute(&mut **tx).await.map_err(schema_error)?;
            let rows = sqlx::query_as::<$db, (i64, String)>("SELECT version, checksum FROM schema_migrations ORDER BY version")
                .fetch_all(&mut **tx).await.map_err(schema_error)?;
            if rows.len() > migrations.len() || rows.iter().zip(migrations).any(|((version, recorded), migration)| *version != migration.version || *recorded != checksum(migration)) {
                return Err(StoreError::Configuration("unsupported or modified database schema; recreate pre-1.x development databases manually"));
            }
            let migrated = rows.len() < migrations.len();
            for migration in migrations.iter().skip(rows.len()) {
                sqlx::raw_sql(&$dialect.migration(migration)).execute(&mut **tx).await.map_err(schema_error)?;
                sqlx::query::<$db>("INSERT INTO schema_migrations (version, checksum) VALUES ($1, $2)")
                    .bind(migration.version).bind(checksum(migration)).execute(&mut **tx).await.map_err(schema_error)?;
            }
            Ok(migrated)
        }
    };
}

runner!(apply_sqlite, sqlx::Sqlite, Dialect::Sqlite);
runner!(apply_postgres, sqlx::Postgres, Dialect::Postgres);

pub(super) async fn sqlite(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<bool, StoreError> {
    apply_sqlite(tx, MIGRATIONS).await
}

pub(super) async fn postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<bool, StoreError> {
    apply_postgres(tx, MIGRATIONS).await
}

#[cfg(test)]
#[path = "../../../tests/migrations.rs"]
mod tests;
