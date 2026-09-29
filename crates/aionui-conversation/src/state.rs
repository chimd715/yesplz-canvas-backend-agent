use std::sync::Arc;

use crate::service::ConversationService;
use aionui_ai_agent::{ActiveLeaseRegistry, IWorkerTaskManager};

/// Shared state for conversation route handlers.
#[derive(Clone)]
pub struct ConversationRouterState {
    pub service: ConversationService,
    pub task_manager: Arc<dyn IWorkerTaskManager>,
    pub active_leases: Arc<ActiveLeaseRegistry>,
}

/// Server-owned dependencies for bootstrap-authenticated archive control routes.
#[derive(Clone)]
pub struct FrozenContinuationRouterState {
    pub service: ConversationService,
    pub bootstrap_secret: Option<Arc<str>>,
    pub runtime_build_sha256: Option<Arc<str>>,
    pub native_session_directory: std::path::PathBuf,
    pub identity_mode: aionui_auth::AuthIdentityMode,
}
