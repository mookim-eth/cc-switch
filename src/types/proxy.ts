export interface ProxyConfig {
  listen_address: string;
  listen_port: number;
  max_retries: number;
  request_timeout: number;
  enable_logging: boolean;
  live_takeover_active?: boolean;
  // 超时配置
  streaming_first_byte_timeout: number;
  streaming_idle_timeout: number;
  non_streaming_timeout: number;
}

export interface ProxyStatus {
  running: boolean;
  address: string;
  port: number;
  active_connections: number;
  total_requests: number;
  success_requests: number;
  failed_requests: number;
  success_rate: number;
  uptime_seconds: number;
  current_provider: string | null;
  current_provider_id: string | null;
  last_request_at: string | null;
  last_error: string | null;
  failover_count: number;
  active_targets?: ActiveTarget[];
}

export interface ActiveTarget {
  app_type: string;
  provider_name: string;
  provider_id: string;
}

export interface ProxyServerInfo {
  address: string;
  port: number;
  started_at: string;
}

export interface ProxyTakeoverStatus {
  claude: boolean;
  "claude-desktop"?: boolean;
  codex: boolean;
  gemini: boolean;
  grokbuild: boolean;
  opencode: boolean;
  openclaw: boolean;
  hermes: boolean;
}

export interface ProviderHealth {
  provider_id: string;
  app_type: string;
  is_healthy: boolean;
  consecutive_failures: number;
  last_success_at: string | null;
  last_failure_at: string | null;
  last_error: string | null;
  updated_at: string;
}

// 熔断器相关类型
export interface CircuitBreakerConfig {
  failureThreshold: number;
  successThreshold: number;
  timeoutSeconds: number;
  errorRateThreshold: number;
  minRequests: number;
}

export type CircuitState = "closed" | "open" | "half_open";

export interface CircuitBreakerStats {
  state: CircuitState;
  consecutiveFailures: number;
  consecutiveSuccesses: number;
  totalRequests: number;
  failedRequests: number;
}

// 供应商健康状态枚举
export enum ProviderHealthStatus {
  Healthy = "healthy",
  Degraded = "degraded",
  Failed = "failed",
  Unknown = "unknown",
}

// 扩展 ProviderHealth 以包含前端计算的状态
export interface ProviderHealthWithStatus extends ProviderHealth {
  status: ProviderHealthStatus;
  circuitState?: CircuitState;
}

export interface ProxyUsageRecord {
  provider_id: string;
  app_type: string;
  endpoint: string;
  request_tokens: number | null;
  response_tokens: number | null;
  status_code: number;
  latency_ms: number;
  error: string | null;
  timestamp: string;
}

// 故障转移队列条目
export interface FailoverQueueItem {
  providerId: string;
  providerName: string;
  providerNotes?: string;
  sortIndex?: number;
}

// 全局代理配置（统一字段，三行镜像）
export interface GlobalProxyConfig {
  proxyEnabled: boolean;
  listenAddress: string;
  listenPort: number;
  enableLogging: boolean;
}

// 应用级代理配置（每个 app 独立）
export interface AppProxyConfig {
  appType: string;
  enabled: boolean;
  autoFailoverEnabled: boolean;
  maxRetries: number;
  streamingFirstByteTimeout: number;
  streamingIdleTimeout: number;
  nonStreamingTimeout: number;
  circuitFailureThreshold: number;
  circuitSuccessThreshold: number;
  circuitTimeoutSeconds: number;
  circuitErrorRateThreshold: number;
  circuitMinRequests: number;
}

export interface LocalControlConfig {
  enabled: boolean;
  tokenConfigured: boolean;
  allowHttpLoopback: boolean;
}

export interface ProxyInteractionRecordingConfig {
  recordBodies: boolean;
  apps: string[];
  models: string[];
  providers: string[];
  maxBodyBytes: number;
  retentionDays: number;
  quotaMb: number;
  recordRawSse: boolean;
}

export interface ProxyHookConfig {
  enabled: boolean;
  endpoint: string;
  bearerToken: string;
  timeoutMs: number;
  maxPayloadBytes: number;
  failClosed: boolean;
  allowRequestReplace: boolean;
  sensitiveStrings: string[];
}

export interface ProxyInteractionFilters {
  appType?: string;
  model?: string;
  providerId?: string;
  statusCode?: number;
  hookHit?: boolean;
  requestId?: string;
  createdAfter?: number;
  createdBefore?: number;
}

export interface ProxyInteractionSummary {
  requestId: string;
  sessionId?: string;
  appType: string;
  clientModel: string;
  outboundModel?: string;
  finalProviderId?: string;
  statusCode?: number;
  isStreaming: boolean;
  hookHit: boolean;
  createdAt: number;
  completedAt?: number;
  retentionUntil: number;
}

export interface ProxyInteractionDetail extends ProxyInteractionSummary {
  requestPayloadRedacted?: string;
  upstreamRequestPayloadRedacted?: string;
  responsePayloadRedacted?: string;
  hookEventsJson: string;
  redactionApplied: boolean;
}

export interface ProxyInteractionAttempt {
  requestId: string;
  attemptIndex: number;
  providerId: string;
  endpointOrigin?: string;
  statusCode?: number;
  errorCode?: string;
  startedAt: number;
  completedAt?: number;
}
