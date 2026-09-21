import { invoke } from "@tauri-apps/api/core";
import type {
  ProxyStatus,
  ProxyServerInfo,
  ProxyTakeoverStatus,
  GlobalProxyConfig,
  AppProxyConfig,
  LocalControlConfig,
  ProxyInteractionAttempt,
  ProxyInteractionDetail,
  ProxyInteractionFilters,
  ProxyInteractionRecordingConfig,
  ProxyInteractionSummary,
  ProxyHookConfig,
} from "@/types/proxy";

export const proxyApi = {
  // ========== 代理服务器控制 API ==========

  // 启动代理服务器
  async startProxyServer(): Promise<ProxyServerInfo> {
    return invoke("start_proxy_server");
  },

  // 停止代理服务器（不恢复已接管配置）
  async stopProxyServer(): Promise<void> {
    return invoke("stop_proxy_server");
  },

  // 停止代理服务器并恢复配置
  async stopProxyWithRestore(): Promise<void> {
    return invoke("stop_proxy_with_restore");
  },

  // 获取代理服务器状态
  async getProxyStatus(): Promise<ProxyStatus> {
    return invoke("get_proxy_status");
  },

  async getLocalControlConfig(): Promise<LocalControlConfig> {
    return invoke("get_local_control_config");
  },

  async setLocalControlEnabled(enabled: boolean): Promise<string | null> {
    return invoke("set_local_control_enabled", { enabled });
  },

  async rotateLocalControlToken(): Promise<string> {
    return invoke("rotate_local_control_token");
  },

  async setLocalControlAllowHttpLoopback(allowed: boolean): Promise<void> {
    return invoke("set_local_control_allow_http_loopback", { allowed });
  },

  async getInteractionRecordingConfig(): Promise<ProxyInteractionRecordingConfig> {
    return invoke("get_proxy_interaction_recording_config");
  },

  async getHookConfig(): Promise<ProxyHookConfig> {
    return invoke("get_proxy_hook_config");
  },

  async saveHookConfig(config: ProxyHookConfig): Promise<void> {
    return invoke("save_proxy_hook_config", { config });
  },

  async saveInteractionRecordingConfig(
    config: ProxyInteractionRecordingConfig,
  ): Promise<void> {
    return invoke("save_proxy_interaction_recording_config", { config });
  },

  async listInteractions(
    filters: ProxyInteractionFilters,
    page = 0,
    pageSize = 50,
  ): Promise<ProxyInteractionSummary[]> {
    return invoke("list_proxy_interactions", { filters, page, pageSize });
  },

  async getInteraction(
    requestId: string,
  ): Promise<ProxyInteractionDetail | null> {
    return invoke("get_proxy_interaction", { requestId, confirmed: true });
  },

  async listInteractionAttempts(
    requestId: string,
  ): Promise<ProxyInteractionAttempt[]> {
    return invoke("list_proxy_interaction_attempts", { requestId });
  },

  // ========== 接管状态 API ==========

  // 获取各应用接管状态
  async getProxyTakeoverStatus(): Promise<ProxyTakeoverStatus> {
    return invoke("get_proxy_takeover_status");
  },

  // 为指定应用开启/关闭接管
  async setProxyTakeoverForApp(
    appType: string,
    enabled: boolean,
  ): Promise<void> {
    return invoke("set_proxy_takeover_for_app", { appType, enabled });
  },

  // ========== v3+ 全局/应用级配置 API ==========

  // 获取全局代理配置
  async getGlobalProxyConfig(): Promise<GlobalProxyConfig> {
    return invoke("get_global_proxy_config");
  },

  // 更新全局代理配置
  async updateGlobalProxyConfig(config: GlobalProxyConfig): Promise<void> {
    return invoke("update_global_proxy_config", { config });
  },

  // 获取指定应用的代理配置
  async getProxyConfigForApp(appType: string): Promise<AppProxyConfig> {
    return invoke("get_proxy_config_for_app", { appType });
  },

  // 更新指定应用的代理配置
  async updateProxyConfigForApp(config: AppProxyConfig): Promise<void> {
    return invoke("update_proxy_config_for_app", { config });
  },

  // ========== 计费默认配置 API ==========

  // 获取默认成本倍率
  async getDefaultCostMultiplier(appType: string): Promise<string> {
    return invoke("get_default_cost_multiplier", { appType });
  },

  // 设置默认成本倍率
  async setDefaultCostMultiplier(
    appType: string,
    value: string,
  ): Promise<void> {
    return invoke("set_default_cost_multiplier", { appType, value });
  },

  // 获取计费模式来源
  async getPricingModelSource(appType: string): Promise<string> {
    return invoke("get_pricing_model_source", { appType });
  },

  // 设置计费模式来源
  async setPricingModelSource(appType: string, value: string): Promise<void> {
    return invoke("set_pricing_model_source", { appType, value });
  },
};
