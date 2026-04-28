use rusqlite::{params, Connection};
use crate::models::ComputeTask;
use std::sync::{Arc, Mutex};

pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    pub fn new(db_path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(db_path)?;

        // Use a composite unique key (orchestrator_pubkey + task_id)
        // to allow different orchestrators to use the same task_id.
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

    /// Saves a new task to the database.
    /// This will fail if the same task_id from the same orchestrator already exists.
    pub fn save_task(&self, task: &ComputeTask) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let payload = serde_json::to_string(task)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

        conn.execute(
            "INSERT INTO tasks (orchestrator_pubkey, task_id, payload, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task.orchestrator_pubkey, task.request.task_id, payload, "pending", now],
        )?;
        Ok(())
    }

    /// Updates the status of a specific task.
    /// Requires both pubkey and task_id to ensure only the owner's task is modified.
    pub fn update_status(
        &self,
        pubkey: &str,
        task_id: &str,
        status: &str
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE tasks SET status = ?1
             WHERE orchestrator_pubkey = ?2 AND task_id = ?3",
            params![status, pubkey, task_id],
        )?;

        if rows == 0 {
            tracing::warn!("No task found for update: {} by {}", task_id, pubkey);
        }
        Ok(())
    }

    /// Retrieves all tasks that are still in 'pending' state.
    /// Used during node startup for recovery.
    pub fn get_pending_tasks(&self) -> anyhow::Result<Vec<ComputeTask>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT payload FROM tasks WHERE status = 'pending'")?;
        let task_iter = stmt.query_map([], |row| {
            let payload: String = row.get(0)?;
            // The stored payload JSON already contains the orchestrator_pubkey
            // thanks to the injection in handlers.rs.
            Ok(serde_json::from_str::<ComputeTask>(&payload).unwrap())
        })?;

        let mut tasks = Vec::new();
        for task in task_iter {
            tasks.push(task?);
        }
        Ok(tasks)
    }
}
