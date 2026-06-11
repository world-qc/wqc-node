use crate::domain::models::ComputeTask;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let path = url.strip_prefix("sqlite:").unwrap_or(url);
        let conn = Connection::open(path)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                orchestrator_pubkey TEXT NOT NULL,
                task_id TEXT NOT NULL,
                payload TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE(orchestrator_pubkey, task_id)
            )",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn save_task(&self, task: &ComputeTask) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let payload = serde_json::to_string(task)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;

        conn.execute(
            "INSERT INTO tasks (orchestrator_pubkey, task_id, payload, status, created_at) VALUES (?1, ?2, ?3, 'pending', ?4)",
            params![task.orchestrator_pubkey, task.request.task_id, payload, now],
        )?;
        Ok(())
    }

    pub fn update_status(
        &self,
        orchestrator_pubkey: &str,
        task_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tasks SET status = ?1 WHERE orchestrator_pubkey = ?2 AND task_id = ?3",
            params![status, orchestrator_pubkey, task_id],
        )?;
        Ok(())
    }

    pub fn get_pending_tasks(&self) -> anyhow::Result<Vec<ComputeTask>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT payload FROM tasks WHERE status = 'pending' ORDER BY created_at ASC",
        )?;

        let tasks = stmt
            .query_map([], |row| {
                let payload: String = row.get(0)?;
                Ok(payload)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::with_capacity(tasks.len());
        for p in tasks {
            out.push(serde_json::from_str(&p)?);
        }
        Ok(out)
    }
}
