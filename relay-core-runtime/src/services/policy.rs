use relay_core_api::policy::{ProxyPolicy, ProxyPolicyPatch};
use crate::audit::AuditActor;
use crate::CoreState;

pub trait PolicyService: Send + Sync {
    fn policy_snapshot(&self) -> ProxyPolicy;
    fn update_policy_from(&self, actor: AuditActor, target: String, policy: ProxyPolicy);
    fn patch_policy_from(&self, actor: AuditActor, target: String, patch: ProxyPolicyPatch);
}

impl PolicyService for CoreState {
    fn policy_snapshot(&self) -> ProxyPolicy {
        CoreState::policy_snapshot(self)
    }

    fn update_policy_from(&self, actor: AuditActor, target: String, policy: ProxyPolicy) {
        CoreState::update_policy_from(self, actor, target, policy)
    }

    fn patch_policy_from(&self, actor: AuditActor, target: String, patch: ProxyPolicyPatch) {
        CoreState::patch_policy_from(self, actor, target, patch)
    }
}
