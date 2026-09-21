use crate::database::Database;
use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::services::{ProxyService, UsageCache};
use std::sync::Arc;

/// 全局应用状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub proxy_service: ProxyService,
    pub usage_cache: Arc<UsageCache>,
    // 内部已使用细粒度锁（accounts/access_tokens/refresh_locks），所有方法均为
    // `&self`，无需外层 RwLock；避免持有粗粒度锁跨网络刷新导致的连锁阻塞。
    pub codex_oauth_manager: Arc<CodexOAuthManager>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(db: Arc<Database>) -> Self {
        let codex_oauth_manager =
            Arc::new(CodexOAuthManager::new(crate::config::get_app_config_dir()));
        let proxy_service =
            ProxyService::new_with_codex_oauth_manager(db.clone(), codex_oauth_manager.clone());

        Self {
            db,
            proxy_service,
            usage_cache: Arc::new(UsageCache::new()),
            codex_oauth_manager,
        }
    }

    /// Build the lightweight state used by the local HTTP control plane.
    ///
    /// The supplied proxy service shares provider-switch locks and OAuth state
    /// with the real service, but deliberately owns an empty server slot. This
    /// avoids a strong reference cycle (`ProxyServer -> AppState -> ProxyService
    /// -> ProxyServer`) while retaining the transactional ProviderService path.
    pub(crate) fn for_local_control(
        db: Arc<Database>,
        proxy_service: ProxyService,
        codex_oauth_manager: Arc<CodexOAuthManager>,
    ) -> Self {
        Self {
            db,
            proxy_service,
            usage_cache: Arc::new(UsageCache::new()),
            codex_oauth_manager,
        }
    }
}
