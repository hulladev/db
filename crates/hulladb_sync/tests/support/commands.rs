use std::path::PathBuf;

use hulladb_postgres::{Session, Store, Uuid};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

/// Real managed commands and client storage, without replication privileges or WAL setup.
pub struct CommandFixture {
    pub store: Store,
    pub session: Session,
    #[allow(dead_code)] // Used by fault-injection cases, not every fixture consumer.
    pub db: Client,
    pub url: String,
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
        let base = std::env::temp_dir().join(format!("hulladb-managed-sync-{id}"));
        Ok(Self {
            store,
            session,
            db,
            url,
            client_path: base.with_extension("client.sqlite"),
            connection,
        })
    }

    pub async fn cleanup(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.store
            .delete_fixture_workspace(self.session.workspace)
            .await?;
        Ok(())
    }
}

impl Drop for CommandFixture {
    fn drop(&mut self) {
        self.connection.abort();
        for path in [&self.client_path] {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}
