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

    /// Source of the script that last loaded. `None` until the first successful load.
    fn current_script(&self) -> Option<String>;
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

    fn current_script(&self) -> Option<String> {
        CoreState::current_script(self)
    }
}
