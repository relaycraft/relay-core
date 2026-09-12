use crate::error::Result;
use relay_core_api::modification::{FlowQuery, FlowSummary};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

/// Seconds since the Unix epoch, in the same unit `flows.created_at` uses.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Current on-disk schema version. Bump this and add an arm to `apply_migration` together.
pub const SCHEMA_VERSION: i64 = 1;

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
        let pool = SqlitePoolOptions::new().connect(url).await?;
        let store = Self { pool };

        // WAL keeps readers (the API serving the UI) from blocking the writer that persists flows,
        // and a busy timeout turns a transient lock into a short wait instead of an immediate error.
        // These are per-connection settings, so they are applied to every pooled connection.
        for pragma in ["PRAGMA journal_mode = WAL;", "PRAGMA busy_timeout = 5000;"] {
            sqlx::query(pragma).execute(&store.pool).await?;
        }

        Ok(store)
    }

    pub async fn init(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rules (
                id TEXT PRIMARY KEY,
                content JSON NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .execute(&self.pool)
        .await?;

        // Optional flow sampling table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS flows (
                id TEXT PRIMARY KEY,
                content JSON NOT NULL,
                created_at INTEGER NOT NULL
            );",
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
            );",
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
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_audit_events_outcome ON audit_events(outcome);",
        )
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
            );",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_start_time_ms ON flow_summaries(start_time_ms DESC);")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_host ON flow_summaries(host);")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_flow_summaries_method ON flow_summaries(method);",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_flow_summaries_status ON flow_summaries(status);",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_flow_summaries_has_error ON flow_summaries(has_error);",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flow_summaries_is_websocket ON flow_summaries(is_websocket);")
            .execute(&self.pool)
            .await?;

        // Schema versioning and migrations.
        //
        // Every statement above is `IF NOT EXISTS`, so an existing database with an older schema is
        // silently left alone: `CREATE TABLE IF NOT EXISTS` cannot add a column, and nothing recorded
        // which shape a file was in. Migrations make the on-disk shape explicit and advanceable.
        self.migrate().await?;

        Ok(())
    }

    /// The underlying connection pool.
    ///
    /// Exposed so integration tests can drive schema state directly (for example stamping a legacy
    /// `user_version`). It is not part of the storage contract and callers outside tests should use
    /// the typed methods.
    pub fn pool_for_tests(&self) -> &SqlitePool {
        &self.pool
    }

    /// Current on-disk schema version.
    pub async fn schema_version(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("PRAGMA user_version;")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Bring the database up to [`SCHEMA_VERSION`], recording each step.
    ///
    /// Migrations run in order and are each committed before the next, so a failure leaves the file
    /// on the last completed version rather than in an unknown state. A fresh database reports
    /// version 0 and simply runs every step.
    pub async fn migrate(&self) -> Result<()> {
        let mut version = self.schema_version().await?;

        if version > SCHEMA_VERSION {
            // Written by a newer build: refusing is safer than silently downgrading the shape.
            return Err(crate::StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }

        while version < SCHEMA_VERSION {
            let next = version + 1;
            self.apply_migration(next).await?;
            // `PRAGMA user_version` cannot be parameterized, but `next` is a local integer constant.
            sqlx::query(&format!("PRAGMA user_version = {next};"))
                .execute(&self.pool)
                .await?;
            version = next;
        }

        Ok(())
    }

    /// Rewrite already-stored flows through `redact`, returning how many rows changed.
    ///
    /// Turning redaction on used to affect only new writes, so a database that had been running
    /// without it kept its original headers, URLs and bodies forever. This closes that gap: an
    /// operator can retroactively apply the policy to existing history.
    ///
    /// Rows are read in pages rather than all at once, so a large history does not have to fit in
    /// memory. `redact` returns the replacement flow for a stored one.
    pub async fn redact_existing_flows<F>(&self, redact: F) -> Result<u64>
    where
        F: Fn(&Value) -> Value,
    {
        const PAGE: i64 = 200;
        let mut changed = 0u64;
        let mut offset = 0i64;

        loop {
            let rows: Vec<(String, Value)> =
                sqlx::query_as("SELECT id, content FROM flows ORDER BY id LIMIT ? OFFSET ?;")
                    .bind(PAGE)
                    .bind(offset)
                    .fetch_all(&self.pool)
                    .await?;

            if rows.is_empty() {
                break;
            }

            for (id, content) in rows {
                let redacted = redact(&content);
                if redacted != content {
                    // `upsert_flow` would restamp `created_at`, which is the row's history; keep it.
                    sqlx::query("UPDATE flows SET content = ? WHERE id = ?;")
                        .bind(&redacted)
                        .bind(&id)
                        .execute(&self.pool)
                        .await?;
                    changed += 1;
                }
            }

            offset += PAGE;
        }

        Ok(changed)
    }

    /// Rewrite already-stored flow summaries through `redact`, returning how many changed.
    pub async fn redact_existing_flow_summaries<F>(&self, redact: F) -> Result<u64>
    where
        F: Fn(&Value) -> Value,
    {
        const PAGE: i64 = 200;
        let mut changed = 0u64;
        let mut offset = 0i64;

        loop {
            let rows: Vec<(String, Value)> = sqlx::query_as(
                "SELECT id, content FROM flow_summaries ORDER BY id LIMIT ? OFFSET ?;",
            )
            .bind(PAGE)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

            if rows.is_empty() {
                break;
            }

            for (id, content) in rows {
                let redacted = redact(&content);
                if redacted != content {
                    sqlx::query("UPDATE flow_summaries SET content = ? WHERE id = ?;")
                        .bind(&redacted)
                        .bind(&id)
                        .execute(&self.pool)
                        .await?;
                    changed += 1;
                }
            }

            offset += PAGE;
        }

        Ok(changed)
    }

    /// Apply a retention policy, deleting the oldest rows beyond its bounds.
    ///
    /// Ordering matters: summaries are pruned by the same keys as flows so the two tables cannot
    /// drift apart, and flows are deleted by id so a summary without its flow (or the reverse) is
    /// not left behind.
    ///
    /// Returns what was removed, so a caller can log or expose it rather than pruning silently.
    pub async fn prune(&self, policy: crate::RetentionPolicy) -> Result<crate::PrunedCounts> {
        let mut counts = crate::PrunedCounts::default();

        if let Some(max_age_secs) = policy.max_age_secs {
            let cutoff = now_secs().saturating_sub(max_age_secs as i64);

            let deleted = sqlx::query("DELETE FROM flows WHERE created_at < ?;")
                .bind(cutoff)
                .execute(&self.pool)
                .await?;
            counts.flows += deleted.rows_affected();

            // Summaries carry their own clock (`start_time_ms`), so they are aged out by it.
            let deleted = sqlx::query("DELETE FROM flow_summaries WHERE start_time_ms < ?;")
                .bind(cutoff.saturating_mul(1000))
                .execute(&self.pool)
                .await?;
            counts.flow_summaries += deleted.rows_affected();
        }

        if let Some(max_flows) = policy.max_flows {
            // Keep the newest `max_flows`; delete anything older than the cutoff row.
            let deleted = sqlx::query(
                "DELETE FROM flows WHERE id NOT IN (
                     SELECT id FROM flows ORDER BY created_at DESC LIMIT ?
                 );",
            )
            .bind(max_flows as i64)
            .execute(&self.pool)
            .await?;
            counts.flows += deleted.rows_affected();

            let deleted = sqlx::query(
                "DELETE FROM flow_summaries WHERE id NOT IN (
                     SELECT id FROM flow_summaries ORDER BY start_time_ms DESC LIMIT ?
                 );",
            )
            .bind(max_flows as i64)
            .execute(&self.pool)
            .await?;
            counts.flow_summaries += deleted.rows_affected();
        }

        if let Some(max_audit_events) = policy.max_audit_events {
            let deleted = sqlx::query(
                "DELETE FROM audit_events WHERE id NOT IN (
                     SELECT id FROM audit_events ORDER BY timestamp_ms DESC LIMIT ?
                 );",
            )
            .bind(max_audit_events as i64)
            .execute(&self.pool)
            .await?;
            counts.audit_events += deleted.rows_affected();
        }

        Ok(counts)
    }

    /// Number of stored flows, for retention reporting and tests.
    pub async fn count_flows(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM flows;")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Number of stored flow summaries.
    pub async fn count_flow_summaries(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM flow_summaries;")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Number of stored audit events.
    pub async fn count_audit_events(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_events;")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    async fn apply_migration(&self, version: i64) -> Result<()> {
        match version {
            // Baseline: the schema created above is version 1. Nothing to alter.
            1 => Ok(()),
            other => Err(crate::StorageError::UnknownMigration { version: other }),
        }
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

        sqlx::query("DELETE FROM rules").execute(&mut *tx).await?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for (id, content) in rules {
            sqlx::query("INSERT INTO rules (id, content, updated_at) VALUES (?, ?, ?)")
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

        sqlx::query("INSERT INTO flows (id, content, created_at) VALUES (?, ?, ?)")
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
                content = excluded.content",
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
             OFFSET ?9",
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
                content = excluded.content",
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
             LIMIT ?6",
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
