use std::path::PathBuf;

use hulladb_cdc::{Capture, Lsn, Source};
use hulladb_postgres::{Session, Store, Uuid};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

/// The real command table, publication and slot; no mirror table or trigger.
pub struct CommandFixture {
    pub store: Store,
    pub session: Session,
    pub db: Client,
    pub url: String,
    pub source: Source,
    pub log_path: PathBuf,
    pub client_path: PathBuf,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl CommandFixture {
    pub async fn create(rows: i32) -> Result<Self, Box<dyn std::error::Error>> {
        let url = std::env::var("HULLADB_TEST_URL")
            .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
        let mut store = Store::connect(&url).await?;
        store.initialize().await?;
        let session = store.create_workspace(Uuid::now_v7()).await?;
        store.seed_fixture(session, rows).await?;
        let (db, connection) = tokio_postgres::connect(&url, NoTls).await?;
        let connection = tokio::spawn(connection);
        db.batch_execute("SET statement_timeout='5s'; SET lock_timeout='5s';")
            .await?;
        let id = Uuid::now_v7().simple().to_string();
        let publication = format!("hccp_{id}");
        let slot = format!("hccs_{id}");
        db.batch_execute(&format!(
            "CREATE PUBLICATION {publication} FOR TABLE _hulladb_task_probe.tasks"
        ))
        .await?;
        let start = Lsn(0);
        let identity = db.query_one("SELECT (pg_control_system()).system_identifier::text, (SELECT oid FROM pg_database WHERE datname=current_database()), (SELECT oid FROM pg_publication WHERE pubname=$1), '_hulladb_task_probe.tasks'::regclass::oid", &[&publication]).await?;
        let source = Source {
            source_id: Uuid::now_v7(),
            system_id: identity.get(0),
            database_oid: identity.get(1),
            publication_oid: identity.get(2),
            slot,
            publication,
            start,
        };
        let base = std::env::temp_dir().join(format!("hulladb-command-cdc-{id}"));
        Ok(Self {
            store,
            session,
            db,
            url,
            source,
            log_path: base.with_extension("log.sqlite"),
            client_path: base.with_extension("client.sqlite"),
            connection,
        })
    }

    pub async fn capture(&mut self) -> Result<Capture, hulladb_cdc::Error> {
        let capture =
            Capture::create(&self.url, &self.source.slot, &self.source.publication).await?;
        self.source = capture.source().clone();
        Ok(capture)
    }

    pub async fn cleanup(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.db
            .query("SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name=$1", &[&self.source.slot])
            .await?;
        self.db
            .batch_execute(&format!("DROP PUBLICATION {}", self.source.publication))
            .await?;
        self.store
            .delete_fixture_workspace(self.session.workspace)
            .await?;
        Ok(())
    }
}

impl Drop for CommandFixture {
    fn drop(&mut self) {
        self.connection.abort();
        for path in [&self.log_path, &self.client_path] {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}
