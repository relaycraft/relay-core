use crate::CoreState;
use crate::audit::AuditActor;
use async_trait::async_trait;

#[async_trait]
pub trait ScriptService: Send + Sync {
    async fn load_script_from(
        &self,
        actor: AuditActor,
        target: String,
        script: &str,
    ) -> Result<(), String>;
}

#[async_trait]
impl ScriptService for CoreState {
    async fn load_script_from(
        &self,
        actor: AuditActor,
        target: String,
        script: &str,
    ) -> Result<(), String> {
        CoreState::load_script_from(self, actor, target, script).await
    }
}
