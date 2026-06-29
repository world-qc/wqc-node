use crate::domain::geo::{GeoInfo, GEO_CACHE_TTL_SECS};
use crate::domain::models::ComputeTask;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct PendingResult {
    pub id: i64,
    pub orchestrator_pubkey: String,
    pub sub_task_id: String,
    pub wire_body: Vec<u8>,
    pub attempts: u32,
}

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

        conn.execute(
            "CREATE TABLE IF NOT EXISTS pending_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                orchestrator_pubkey TEXT NOT NULL,
                sub_task_id TEXT NOT NULL,
                wire_body BLOB NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                UNIQUE(orchestrator_pubkey, sub_task_id)
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS geo_cache (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                latitude REAL NOT NULL,
                longitude REAL NOT NULL,
                country TEXT NOT NULL,
                city TEXT NOT NULL,
                updated_at INTEGER NOT NULL
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
        let now = unix_now();

        conn.execute(
            "INSERT INTO tasks (orchestrator_pubkey, task_id, payload, status, created_at) VALUES (?1, ?2, ?3, 'pending', ?4)",
            params![task.orchestrator_pubkey, task.request.task_id, payload, now],
        )?;
        Ok(())
    }

    pub fn load_task(
        &self,
        orchestrator_pubkey: &str,
        task_id: &str,
    ) -> anyhow::Result<Option<ComputeTask>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT payload FROM tasks WHERE orchestrator_pubkey = ?1 AND task_id = ?2",
        )?;

        let mut rows = stmt.query(params![orchestrator_pubkey, task_id])?;
        if let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            return Ok(Some(serde_json::from_str(&payload)?));
        }
        Ok(None)
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

    pub fn upsert_pending_result(
        &self,
        orchestrator_pubkey: &str,
        sub_task_id: &str,
        wire_body: &[u8],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = unix_now();
        conn.execute(
            "INSERT INTO pending_results (orchestrator_pubkey, sub_task_id, wire_body, attempts, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(orchestrator_pubkey, sub_task_id) DO UPDATE SET
               wire_body = excluded.wire_body,
               attempts = 0,
               created_at = excluded.created_at",
            params![orchestrator_pubkey, sub_task_id, wire_body, now],
        )?;
        Ok(())
    }

    pub fn delete_pending_result(
        &self,
        orchestrator_pubkey: &str,
        sub_task_id: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM pending_results WHERE orchestrator_pubkey = ?1 AND sub_task_id = ?2",
            params![orchestrator_pubkey, sub_task_id],
        )?;
        Ok(())
    }

    pub fn list_pending_results(&self) -> anyhow::Result<Vec<PendingResult>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, orchestrator_pubkey, sub_task_id, wire_body, attempts
             FROM pending_results
             ORDER BY created_at ASC",
        )?;

        let rows = stmt
            .query_map([], |row| {
                Ok(PendingResult {
                    id: row.get(0)?,
                    orchestrator_pubkey: row.get(1)?,
                    sub_task_id: row.get(2)?,
                    wire_body: row.get(3)?,
                    attempts: row.get::<_, i64>(4)? as u32,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    pub fn count_pending_results(&self) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM pending_results", [], |row| {
            row.get(0)
        })?;
        Ok(count as usize)
    }

    pub fn increment_pending_result_attempts(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE pending_results SET attempts = attempts + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Returns cached geo when `updated_at` is within the last 24 hours.
    pub fn get_cached_geo(&self) -> anyhow::Result<Option<GeoInfo>> {
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT latitude, longitude, country, city FROM geo_cache
             WHERE id = 1 AND (?1 - updated_at) < ?2",
        )?;
        let mut rows = stmt.query(params![now, GEO_CACHE_TTL_SECS])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(GeoInfo {
                latitude: row.get(0)?,
                longitude: row.get(1)?,
                country: row.get(2)?,
                city: row.get(3)?,
            }));
        }
        Ok(None)
    }

    pub fn save_geo_cache(&self, geo: &GeoInfo) -> anyhow::Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO geo_cache (id, latitude, longitude, country, city, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
               latitude = excluded.latitude,
               longitude = excluded.longitude,
               country = excluded.country,
               city = excluded.city,
               updated_at = excluded.updated_at",
            params![geo.latitude, geo.longitude, geo.country, geo.city, now],
        )?;
        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::{ComputeRequest, ComputeTask};

    fn sample_task(task_id: &str) -> ComputeTask {
        ComputeTask {
            request: ComputeRequest {
                task_id: task_id.to_string(),
                parent_task_id: Some("parent-1".to_string()),
                circuit_id: "circuit-1".to_string(),
                node_id: None,
                qubit_count: 2,
                original_qubit_count: 3,
                slice_id: "0".to_string(),
                slice_assignments: vec![],
                circuit: vec![],
                required_votes: Some(2),
                mps_max_bond_dim: None,
                output_mode: String::new(),
                classical_bit_count: None,
                shots: None,
                sample_seed: None,
                observables: vec![],
            },
            orchestrator_pubkey: "orch-pubkey".to_string(),
        }
    }

    #[test]
    fn geo_cache_round_trip_within_ttl() {
        use crate::domain::geo::GeoInfo;

        let storage = Storage::new(":memory:").unwrap();
        let geo = GeoInfo {
            latitude: 35.6762,
            longitude: 139.6503,
            country: "Japan".into(),
            city: "Tokyo".into(),
        };
        storage.save_geo_cache(&geo).unwrap();
        let loaded = storage.get_cached_geo().unwrap().expect("cached geo");
        assert_eq!(loaded, geo);
    }

    #[test]
    fn load_task_returns_none_for_missing_row() {
        let storage = Storage::new(":memory:").unwrap();
        assert!(storage
            .load_task("orch-pubkey", "missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn load_task_round_trip() {
        let storage = Storage::new(":memory:").unwrap();
        let task = sample_task("sub-1");
        storage.save_task(&task).unwrap();

        let loaded = storage
            .load_task("orch-pubkey", "sub-1")
            .unwrap()
            .expect("task row");
        assert_eq!(loaded.request.task_id, "sub-1");
    }

    #[test]
    fn pending_result_outbox_round_trip() {
        let storage = Storage::new(":memory:").unwrap();
        let body = br#"{"sub_task_id":"sub-1"}"#.to_vec();

        storage
            .upsert_pending_result("orch-pubkey", "sub-1", &body)
            .unwrap();
        assert_eq!(storage.count_pending_results().unwrap(), 1);

        let pending = storage.list_pending_results().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sub_task_id, "sub-1");
        assert_eq!(pending[0].wire_body, body);

        storage
            .delete_pending_result("orch-pubkey", "sub-1")
            .unwrap();
        assert_eq!(storage.count_pending_results().unwrap(), 0);
    }

    #[test]
    fn upsert_pending_result_replaces_wire_body() {
        let storage = Storage::new(":memory:").unwrap();
        storage
            .upsert_pending_result("orch-pubkey", "sub-1", b"v1")
            .unwrap();
        storage
            .upsert_pending_result("orch-pubkey", "sub-1", b"v2")
            .unwrap();

        let pending = storage.list_pending_results().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].wire_body, b"v2");
        assert_eq!(pending[0].attempts, 0);
    }

    #[test]
    fn task_status_update_does_not_touch_outbox() {
        let storage = Storage::new(":memory:").unwrap();
        let task = sample_task("sub-1");
        storage.save_task(&task).unwrap();
        storage
            .upsert_pending_result("orch-pubkey", "sub-1", b"body")
            .unwrap();

        storage
            .update_status("orch-pubkey", "sub-1", "completed")
            .unwrap();

        assert_eq!(storage.count_pending_results().unwrap(), 1);
        let pending_tasks = storage.get_pending_tasks().unwrap();
        assert!(pending_tasks.is_empty());
    }
}
