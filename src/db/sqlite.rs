use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteSynchronous},
    SqlitePool,
};

use super::{DbError, DbResult};

use crate::config::DbConfig;

pub mod shared {
    use std::ops::RangeInclusive;

    use hrpc::exports::futures_util::StreamExt;
    use itertools::Either;
    use smol_str::SmolStr;
    use sqlx::Row;

    use crate::{db::Batch, utils::evec::EVec};

    use super::*;

    pub async fn open_database(db_path: String, _db_config: DbConfig) -> DbResult<Db> {
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(true)
                .synchronous(SqliteSynchronous::Normal),
        )
        .await?;

        Ok(Db { pool })
    }

    #[derive(Debug, Clone)]
    pub struct Db {
        pool: SqlitePool,
    }

    impl Db {
        pub async fn open_tree(&self, name: &[u8]) -> DbResult<Tree> {
            let name = std::str::from_utf8(name).map_err(|err| DbError {
                inner: Box::new(err),
            })?;

            let mut conn = self.pool.acquire().await?;
            sqlx::query(&format!(
                "CREATE TABLE IF NOT EXISTS {} ( \"key\" BLOB PRIMARY KEY, \"value\" BLOB NOT NULL );",
                name
            ))
            .execute(&mut conn)
            .await?;

            Ok(Tree {
                pool: self.pool.clone(),
                get_query: format!("SELECT value FROM {} WHERE key = ?", name).into(),
                insert_query: format!("INSERT OR REPLACE INTO {} (key, value) VALUES (?, ?)", name)
                    .into(),
                remove_query: format!("DELETE FROM {} WHERE key = ?", name).into(),
                contains_key_query: format!(
                    "SELECT EXISTS (SELECT value FROM {} WHERE key = ?)",
                    name
                )
                .into(),
                iter_from_query: format!(
                    "SELECT key, value FROM {} WHERE key >= ? ORDER BY key ASC",
                    name
                )
                .into(),
                iter_query: format!("SELECT key, value FROM {} ORDER BY key ASC", name).into(),
            })
        }

        pub async fn flush(&self) -> DbResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    pub struct Tree {
        pool: SqlitePool,
        get_query: SmolStr,
        insert_query: SmolStr,
        remove_query: SmolStr,
        contains_key_query: SmolStr,
        iter_from_query: SmolStr,
        iter_query: SmolStr,
    }

    impl Tree {
        pub async fn get(&self, key: &[u8]) -> DbResult<Option<EVec>> {
            let mut conn = self.pool.acquire().await?;

            let val = sqlx::query(self.get_query.as_str())
                .bind(key)
                .fetch_optional(&mut conn)
                .await?;

            Ok(val.map(|r| EVec::Owned(r.get::<Vec<u8>, _>(0))))
        }

        pub async fn insert(&self, key: &[u8], value: impl AsRef<[u8]>) -> DbResult<Option<EVec>> {
            let mut conn = self.pool.acquire().await?;

            let val = sqlx::query(self.insert_query.as_str())
                .bind(key)
                .bind(value.as_ref())
                .fetch_optional(&mut conn)
                .await?;

            Ok(val.map(|r| EVec::Owned(r.get::<Vec<u8>, _>(0))))
        }

        pub async fn remove(&self, key: &[u8]) -> DbResult<Option<EVec>> {
            let mut conn = self.pool.acquire().await?;

            let val = sqlx::query(self.remove_query.as_str())
                .bind(key)
                .fetch_optional(&mut conn)
                .await?;

            Ok(val.map(|r| EVec::Owned(r.get::<Vec<u8>, _>(0))))
        }

        pub async fn contains_key(&self, key: &[u8]) -> DbResult<bool> {
            let mut conn = self.pool.acquire().await?;

            let row = sqlx::query(self.contains_key_query.as_str())
                .bind(key)
                .fetch_one(&mut conn)
                .await?;

            Ok(row.get(0))
        }

        pub async fn apply_batch(&self, batch: Batch) -> DbResult<()> {
            let mut txn = self.pool.begin().await?;

            for (key, val) in batch.inserts {
                if let Some(value) = val {
                    sqlx::query(self.insert_query.as_str())
                        .bind(key.as_ref())
                        .bind(value.as_ref())
                        .execute(&mut txn)
                        .await?;
                } else {
                    sqlx::query(self.remove_query.as_str())
                        .bind(key.as_ref())
                        .execute(&mut txn)
                        .await?;
                }
            }

            txn.commit().await?;

            Ok(())
        }

        pub async fn iter(&self) -> impl Iterator<Item = DbResult<(EVec, EVec)>> {
            let mut conn = match self.pool.acquire().await {
                Ok(conn) => conn,
                Err(err) => return Either::Left(std::iter::once(Err(err.into()))),
            };

            let row_query = sqlx::query(self.iter_query.as_str()).fetch_all(&mut conn);

            let rows = match row_query.await {
                Ok(rows) => rows,
                Err(err) => return Either::Left(std::iter::once(Err(err.into()))),
            };

            Either::Right(
                rows.into_iter()
                    .map(|r| Ok((EVec::Owned(r.get(0)), EVec::Owned(r.get(1))))),
            )
        }

        pub async fn scan_prefix(
            &self,
            prefix: &[u8],
        ) -> impl Iterator<Item = DbResult<(EVec, EVec)>> {
            let mut conn = match self.pool.acquire().await {
                Ok(conn) => conn,
                Err(err) => return Either::Left(std::iter::once(Err(err.into()))),
            };

            let mut stream = sqlx::query(self.iter_from_query.as_str())
                .bind(prefix)
                .fetch(&mut conn);

            let mut items = Vec::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(row) => {
                        let key: Vec<u8> = row.get(0);
                        if key.starts_with(prefix) {
                            let value: Vec<u8> = row.get(1);
                            items.push(Ok((EVec::Owned(key), EVec::Owned(value))));
                        } else {
                            break;
                        }
                    }
                    Err(err) => {
                        items.push(Err(err.into()));
                    }
                }
            }

            Either::Right(items.into_iter())
        }

        pub async fn range(
            &self,
            range: RangeInclusive<&[u8]>,
        ) -> impl Iterator<Item = DbResult<(EVec, EVec)>> + DoubleEndedIterator {
            let mut conn = match self.pool.acquire().await {
                Ok(conn) => conn,
                Err(err) => return Either::Left(std::iter::once(Err(err.into()))),
            };

            let start_key = range.start();
            let end_key = range.end();

            let mut stream = sqlx::query(self.iter_from_query.as_str())
                .bind(start_key)
                .fetch(&mut conn);

            let mut items = Vec::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(row) => {
                        let key: Vec<u8> = row.get(0);
                        if key.as_slice() <= *end_key {
                            let value: Vec<u8> = row.get(1);
                            items.push(Ok((EVec::Owned(key), EVec::Owned(value))));
                        } else {
                            break;
                        }
                    }
                    Err(err) => {
                        items.push(Err(err.into()));
                    }
                }
            }

            Either::Right(items.into_iter())
        }

        pub async fn verify_integrity(&self) -> DbResult<()> {
            Ok(())
        }
    }
}

impl From<sqlx::Error> for DbError {
    fn from(err: sqlx::Error) -> Self {
        DbError {
            inner: Box::new(err),
        }
    }
}
