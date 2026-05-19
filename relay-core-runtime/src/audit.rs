use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditActor {
    Runtime,
    Tauri,
    Http,
    Probe,
    Cli,
}

impl AuditActor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Tauri => "tauri",
            Self::Http => "http",
            Self::Probe => "probe",
            Self::Cli => "cli",
        }
    }
}

impl FromStr for AuditActor {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "runtime" => Ok(Self::Runtime),
            "tauri" => Ok(Self::Tauri),
            "http" => Ok(Self::Http),
            "probe" => Ok(Self::Probe),
            "cli" => Ok(Self::Cli),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventKind {
    RuleChanged,
    InterceptResolved,
    ScriptReloaded,
    PolicyUpdated,
}

impl AuditEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RuleChanged => "rule_changed",
            Self::InterceptResolved => "intercept_resolved",
            Self::ScriptReloaded => "script_reloaded",
            Self::PolicyUpdated => "policy_updated",
        }
    }
}

impl FromStr for AuditEventKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rule_changed" => Ok(Self::RuleChanged),
            "intercept_resolved" => Ok(Self::InterceptResolved),
            "script_reloaded" => Ok(Self::ScriptReloaded),
            "policy_updated" => Ok(Self::PolicyUpdated),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Success,
    Failed,
}

impl AuditOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for AuditOutcome {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: String,
    pub timestamp_ms: u64,
    pub actor: AuditActor,
    pub kind: AuditEventKind,
    pub target: String,
    pub outcome: AuditOutcome,
    pub details: Value,
}

impl AuditEvent {
    pub fn new(
        actor: AuditActor,
        kind: AuditEventKind,
        target: impl Into<String>,
        outcome: AuditOutcome,
        details: Value,
    ) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: Uuid::new_v4().to_string(),
            timestamp_ms,
            actor,
            kind,
            target: target.into(),
            outcome,
            details,
        }
    }
}
