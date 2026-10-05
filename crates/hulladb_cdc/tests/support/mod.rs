use std::{path::PathBuf, time::Duration};

use hulladb_cdc::{Error, Source};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

pub struct Fixture {
    pub db: Client,
    pub url: String,
    pub source: Source,
    pub schema: String,
    pub path: PathBuf,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl Fixture {
    pub async fn create() -> Result<Self, Error> {
        Self::create_with_seed(false).await
    }

    pub async fn create_with_seed(seed: bool) -> Result<Self, Error> {
        let url = std::env::var("HULLADB_TEST_URL")
            .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
        let mut config: tokio_postgres::Config = url.parse()?;
        config.connect_timeout(Duration::from_secs(5));
        let (db, connection) = config.connect(NoTls).await?;
        let connection = tokio::spawn(connection);
        db.batch_execute("SET statement_timeout='5s'; SET lock_timeout='5s';")
            .await?;
        let id = Uuid::now_v7().simple().to_string();
        let schema = format!("hcdc_{id}");
        let publication = format!("hcp_{id}");
        let slot = format!("hcs_{id}");
        // All identifiers are generated locally; no user SQL is interpolated.
        db.batch_execute(&format!("CREATE SCHEMA {schema};
            CREATE TABLE {schema}.tasks(id bigint PRIMARY KEY, title text NOT NULL, completed boolean NOT NULL DEFAULT false);
            CREATE TABLE {schema}.unpublished(id bigint PRIMARY KEY);
            CREATE PUBLICATION {publication} FOR TABLE {schema}.tasks;")).await?;
        if seed {
            db.batch_execute(&format!(
                "INSERT INTO {schema}.tasks VALUES(0,'before slot',false)"
            ))
            .await?;
        }
        let row = db
            .query_one(
                "SELECT lsn::text FROM pg_create_logical_replication_slot($1,'pgoutput')",
                &[&slot],
            )
            .await?;
        let start = row.get::<_, String>(0).parse()?;
        let row = db.query_one("SELECT (pg_control_system()).system_identifier::text, (SELECT oid FROM pg_database WHERE datname=current_database()), (SELECT oid FROM pg_publication WHERE pubname=$1)", &[&publication]).await?;
        let source = Source {
            generation: Uuid::now_v7(),
            system_id: row.get(0),
            database_oid: row.get(1),
            publication_oid: row.get(2),
            slot,
            publication,
            start,
        };
        let path = std::env::temp_dir().join(format!("hulladb-cdc-{id}.sqlite"));
        Ok(Self {
            db,
            url,
            source,
            schema,
            path,
            connection,
        })
    }

    pub async fn cleanup(self) -> Result<(), Error> {
        self.db
            .query_one("SELECT pg_drop_replication_slot($1)", &[&self.source.slot])
            .await?;
        self.db
            .batch_execute(&format!(
                "DROP PUBLICATION {}; DROP SCHEMA {} CASCADE",
                self.source.publication, self.schema
            ))
            .await?;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.connection.abort();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
