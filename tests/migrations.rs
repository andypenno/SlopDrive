use super::*;
use crate::test_helpers::{TestApp, TestDatabase};

const EXTRA: Table = Table {
    name: "migration_probe",
    columns: &[Column {
        name: "value",
        data_type: DataType::Integer,
        nullable: false,
    }],
    constraints: &[],
};
const SECOND: Migration = Migration {
    version: 2,
    tables: &[EXTRA],
};

#[test]
fn fingerprint_tracks_logical_schema_and_not_provider_sql() {
    let baseline = v001::BASELINE;
    assert_ne!(
        Dialect::Sqlite.migration(&baseline),
        Dialect::Postgres.migration(&baseline)
    );
    assert_eq!(
        checksum(&baseline),
        "f0c1ca950134b6c5eba3d91e1ecfbf315115373bcb6d21764b336841ff600dd8"
    );
    assert_ne!(
        checksum(&baseline),
        checksum(&Migration {
            version: 2,
            ..baseline
        })
    );
    assert_ne!(
        checksum(&SECOND),
        checksum(&Migration {
            version: 2,
            tables: &[]
        })
    );
}

#[tokio::test]
async fn pending_migrations_are_atomic_repeatable_and_validate_the_ledger() {
    let fixture = TestApp::new().await;
    macro_rules! check {
        ($pool:expr, $db:ty, $apply:ident) => {{
            let pool = $pool;
            let mut tx = pool.begin().await.unwrap();
            let broken = Migration {
                version: 3,
                tables: &[EXTRA],
            };
            assert!(
                $apply(&mut tx, &[v001::BASELINE, SECOND, broken])
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
            assert_eq!(
                sqlx::query_scalar::<$db, i64>("SELECT COUNT(*) FROM schema_migrations")
                    .fetch_one(pool)
                    .await
                    .unwrap(),
                1
            );
            // A successful retry proves the first CREATE TABLE was rolled back too.
            let registry = [v001::BASELINE, SECOND];
            let mut tx = pool.begin().await.unwrap();
            assert!($apply(&mut tx, &registry).await.unwrap());
            sqlx::query::<$db>("INSERT INTO migration_probe VALUES (42)")
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let recorded: Vec<(i64, String)> = sqlx::query_as::<$db, (i64, String)>(
                "SELECT version, checksum FROM schema_migrations ORDER BY version",
            )
            .fetch_all(pool)
            .await
            .unwrap();
            assert_eq!(
                recorded,
                vec![(1, checksum(&v001::BASELINE)), (2, checksum(&SECOND))]
            );
            for sql in [
                "DELETE FROM schema_migrations",
                "DROP TABLE schema_migrations; CREATE TABLE schema_migrations (wrong INTEGER)",
            ] {
                let mut tx = pool.begin().await.unwrap();
                sqlx::raw_sql(sql).execute(&mut *tx).await.unwrap();
                assert!(matches!(
                    $apply(&mut tx, &registry).await,
                    Err(StoreError::Configuration(_))
                ));
                tx.rollback().await.unwrap();
            }
            let mut tx = pool.begin().await.unwrap();
            assert!(!$apply(&mut tx, &registry).await.unwrap());
            assert!($apply(&mut tx, MIGRATIONS).await.is_err());
            tx.rollback().await.unwrap();
            assert_eq!(
                sqlx::query_scalar::<$db, i64>("SELECT value FROM migration_probe")
                    .fetch_one(pool)
                    .await
                    .unwrap(),
                42
            );
            for sql in [
                "UPDATE schema_migrations SET checksum = 'old SQL checksum' WHERE version = 1",
                "UPDATE schema_migrations SET version = 3 WHERE version = 2",
                "DELETE FROM schema_migrations WHERE version = 1",
            ] {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query::<$db>(sql).execute(&mut *tx).await.unwrap();
                assert!($apply(&mut tx, &registry).await.is_err(), "accepted {sql}");
                tx.rollback().await.unwrap();
            }
            let mut tx = pool.begin().await.unwrap();
            assert!(
                $apply(
                    &mut tx,
                    &[
                        v001::BASELINE,
                        Migration {
                            version: 3,
                            ..SECOND
                        }
                    ]
                )
                .await
                .is_err()
            );
            tx.rollback().await.unwrap();
        }};
    }
    match &fixture.db {
        TestDatabase::Sqlite(pool) => check!(pool, sqlx::Sqlite, apply_sqlite),
        TestDatabase::Postgres(pool) => check!(pool, sqlx::Postgres, apply_postgres),
    }
}

#[tokio::test]
async fn baseline_constraints_enforce_common_storage_contract() {
    let fixture = TestApp::with_content(Default::default()).await;
    let saved = fixture
        .state
        .storage
        .create(b"<p>original</p>")
        .await
        .unwrap();
    for size in [1, 1048576] {
        fixture.db.execute(&format!(
            "BEGIN; UPDATE versions SET size_bytes = {size}, content = NULL, object_key = 'probe'; ROLLBACK"
        )).await.unwrap();
    }
    for assignment in [
        "size_bytes = 0, content = NULL, object_key = 'probe'",
        "size_bytes = 1048577, content = NULL, object_key = 'probe'",
        "version_number = 0",
        "content = NULL",
        "object_key = 'both-locations'",
        "content = NULL, object_key = ''",
        "content = X'00'",
        "upload_id = 'missing'",
    ] {
        let assignment = if matches!(fixture.db, TestDatabase::Postgres(_)) {
            assignment.replace("X'00'", "decode('00', 'hex')")
        } else {
            assignment.to_owned()
        };
        assert!(
            fixture
                .db
                .execute(&format!("UPDATE versions SET {assignment}"))
                .await
                .is_err(),
            "accepted {assignment}"
        );
    }
    for digest in [
        "A".repeat(64),
        "g".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        assert!(fixture.db.execute(&format!(
            "INSERT INTO versions SELECT upload_id, '{digest}', created_at, size_bytes, version_number + 1, content, object_key FROM versions"
        )).await.is_err(), "accepted hash {digest}");
    }
    assert!(fixture.db.execute(&format!(
        "INSERT INTO versions SELECT upload_id, '{}', created_at, size_bytes, version_number, content, object_key FROM versions",
        "a".repeat(64)
    )).await.is_err());
    let nul = match fixture.db {
        TestDatabase::Sqlite(_) => "char(0)",
        TestDatabase::Postgres(_) => "chr(0)",
    };
    for value in ["'A'", "'é'", "''", "NULL"] {
        assert!(
            fixture
                .db
                .execute(&format!(
                    "INSERT INTO api_keys VALUES ({value}, '{}')",
                    "b".repeat(64)
                ))
                .await
                .is_err()
        );
    }
    for value in [
        format!("'{}' || {nul} || 'a'", "a".repeat(16)),
        format!("'{}é'", "a".repeat(15)),
        format!("'{}A'", "a".repeat(15)),
    ] {
        assert!(
            fixture
                .db
                .execute(&format!(
                    "INSERT INTO api_keys VALUES ({value}, '{}')",
                    "b".repeat(64)
                ))
                .await
                .is_err()
        );
    }
    for sql in [
        format!(
            "INSERT INTO content_store VALUES (2, 'identity', '{}')",
            "a".repeat(32)
        ),
        "INSERT INTO content_store VALUES (1, 'identity', 'BAD')".into(),
    ] {
        assert!(fixture.db.execute(&sql).await.is_err());
    }
    assert_eq!(
        fixture
            .state
            .storage
            .get(&saved.upload_id, None)
            .await
            .unwrap(),
        b"<p>original</p>"
    );
}
