use sqlx::{SqlitePool, Row};
use sqlx::sqlite::SqlitePoolOptions;
use crate::error::Result;
use serde_json::Value;
use relay_core_api::modification::{FlowQuery, FlowSummary};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

pub struct AuditEventRecord<'a> {
    pub id: &'a str,
    pub timestamp_ms: u64,
    pub actor: &'a str,
    pub kind: &'a str,
    pub target: &'a str,
    pub outcome: &'a str,
    pub content: &'a Value,
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn init(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rules (
                id TEXT PRIMARY KEY,
                content JSON NOT NULL,
                updated_at INTEGER NOT NULL
            );"
        )
        .execute(&self.pool)
        .await?;

        // Optional flow sampling table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS flows (
                id TEXT PRIMARY KEY,
                content JSON NOT NULL,
                created_at INTEGER NOT NULL
            );"
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS audit_events (
                id TEXT PRIMARY KEY,
                timestamp_ms INTEGER NOT NULL,
                actor TEXT NOT NULL,
                kind TEXT NOT NULL,
                target TEXT NOT NULL,
                outcome TEXT NOT NULL,
                content JSON NOT NULL
            );"
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp_ms ON audit_events(timestamp_ms DESC);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_events_actor ON audit_events(actor);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_events_kind ON audit_events(kind);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_events_outcome ON audit_events(outcome);")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS flow_summaries (
                id TEXT PRIMARY KEY,
                start_time_ms INTEGER NOT NULL,
                method TEXT NOT NULL,
                host TEXT NOT NULL,
                path TEXT NOT NULL,
                status INTEGER,
                has_error INTEGER NOT NULL,
                is_websocket INTEGER NOT NULL,
                content JSON NOT NULL
            );"
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_start_time_ms ON flow_summaries(start_time_ms DESC);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_host ON flow_summaries(host);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_method ON flow_summaries(method);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_status ON flow_summaries(status);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_has_error ON flow_summaries(has_error);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_is_websocket ON flow_summaries(is_websocket);")
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn save_rule(&self, id: &str, content: &Value) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        
        sqlx::query(
            "INSERT INTO rules (id, content, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at"
        )
        .bind(id)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;
        
        Ok(())
    }

    pub async fn delete_rule(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rules WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn load_rules(&self) -> Result<Vec<(String, Value)>> {
        let rows = sqlx::query("SELECT id, content FROM rules")
            .fetch_all(&self.pool)
            .await?;
        
        let mut rules = Vec::new();
        for row in rows {
            let id: String = row.get("id");
            let content: Value = row.get("content");
            rules.push((id, content));
        }
        Ok(rules)
    }

    pub async fn replace_rules(&self, rules: &[(String, Value)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        
        sqlx::query("DELETE FROM rules")
            .execute(&mut *tx)
            .await?;
            
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for (id, content) in rules {
            sqlx::query(
                "INSERT INTO rules (id, content, updated_at) VALUES (?, ?, ?)"
            )
            .bind(id)
            .bind(content)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        
        tx.commit().await?;
        Ok(())
    }

    // Optional sampling
    pub async fn save_flow(&self, id: &str, content: &Value) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO flows (id, content, created_at) VALUES (?, ?, ?)"
        )
        .bind(id)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_flow(&self, id: &str, content: &Value) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO flows (id, content, created_at) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET content = excluded.content, created_at = excluded.created_at"
        )
        .bind(id)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_flow(&self, id: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT content FROM flows WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("content")))
    }

    pub async fn upsert_flow_summary(&self, summary: &FlowSummary) -> Result<()> {
        let content = serde_json::to_value(summary).unwrap_or_default();
        sqlx::query(
            "INSERT INTO flow_summaries (
                id, start_time_ms, method, host, path, status, has_error, is_websocket, content
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                start_time_ms = excluded.start_time_ms,
                method = excluded.method,
                host = excluded.host,
                path = excluded.path,
                status = excluded.status,
                has_error = excluded.has_error,
                is_websocket = excluded.is_websocket,
                content = excluded.content"
        )
        .bind(&summary.id)
        .bind(summary.start_time_ms)
        .bind(&summary.method)
        .bind(&summary.host)
        .bind(&summary.path)
        .bind(summary.status.map(i64::from))
        .bind(if summary.has_error { 1i64 } else { 0i64 })
        .bind(if summary.is_websocket { 1i64 } else { 0i64 })
        .bind(content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn query_flow_summaries(&self, query: &FlowQuery) -> Result<Vec<FlowSummary>> {
        let limit = query.limit.unwrap_or(50).clamp(1, 200) as i64;
        let offset = query.offset.unwrap_or(0) as i64;
        let rows = sqlx::query(
            "SELECT content
             FROM flow_summaries
             WHERE (?1 IS NULL OR host LIKE '%' || ?1 || '%')
               AND (?2 IS NULL OR path LIKE '%' || ?2 || '%')
               AND (?3 IS NULL OR lower(method) = lower(?3))
               AND (?4 IS NULL OR status >= ?4)
               AND (?5 IS NULL OR status <= ?5)
               AND (?6 IS NULL OR has_error = ?6)
               AND (?7 IS NULL OR is_websocket = ?7)
             ORDER BY start_time_ms DESC
             LIMIT ?8
             OFFSET ?9"
        )
        .bind(query.host.as_deref())
        .bind(query.path_contains.as_deref())
        .bind(query.method.as_deref())
        .bind(query.status_min.map(i64::from))
        .bind(query.status_max.map(i64::from))
        .bind(query.has_error.map(|v| if v { 1i64 } else { 0i64 }))
        .bind(query.is_websocket.map(|v| if v { 1i64 } else { 0i64 }))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let content: Value = row.get("content");
            if let Ok(summary) = serde_json::from_value::<FlowSummary>(content) {
                summaries.push(summary);
            }
        }
        Ok(summaries)
    }

    pub async fn save_audit_event(&self, event: AuditEventRecord<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_events (id, timestamp_ms, actor, kind, target, outcome, content)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                timestamp_ms = excluded.timestamp_ms,
                actor = excluded.actor,
                kind = excluded.kind,
                target = excluded.target,
                outcome = excluded.outcome,
                content = excluded.content"
        )
        .bind(event.id)
        .bind(event.timestamp_ms as i64)
        .bind(event.actor)
        .bind(event.kind)
        .bind(event.target)
        .bind(event.outcome)
        .bind(event.content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn query_audit_events(
        &self,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
        actor: Option<&str>,
        kind: Option<&str>,
        outcome: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT content
             FROM audit_events
             WHERE (?1 IS NULL OR timestamp_ms >= ?1)
               AND (?2 IS NULL OR timestamp_ms <= ?2)
               AND (?3 IS NULL OR actor = ?3)
               AND (?4 IS NULL OR kind = ?4)
               AND (?5 IS NULL OR outcome = ?5)
             ORDER BY timestamp_ms DESC
             LIMIT ?6"
        )
        .bind(since_ms.map(|v| v as i64))
        .bind(until_ms.map(|v| v as i64))
        .bind(actor)
        .bind(kind)
        .bind(outcome)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let content: Value = row.get("content");
            events.push(content);
        }
        Ok(events)
    }
}
