# CC Switch 远程配置同步方案

## 1. 目标

实现以下链路：

```text
配置服务
   │ HTTPS：下发当前上游配置
   ▼
本地同步小程序
   │ localhost + Bearer Token
   ▼
CC Switch 控制 API
   │ 调用现有 ProviderService
   ▼
CC Switch 本地代理
   │ 动态注入实际上游 URL / API Key
   ▼
Claude Code / Codex / Gemini CLI / Grok Build
```

用户只需首次在 CC Switch 中启用代理服务和对应应用接管。之后服务端变更供应商、上游 URL 或 API Key 时，本地同步小程序自动应用，正在运行的 CLI 无需重启。

## 2. 核心结论

- 不开发新的反向代理，复用 CC Switch 已有的 Axum 代理、协议转换、请求日志、故障转移和热切换能力。
- 不使用 Deeplink 做自动同步。Deeplink 面向交互式导入，需要用户确认，会生成新的 Provider ID，而且不适合在 URL 中传递 API Key。
- 不建设插件系统。当前 CC Switch 没有供第三方扩展内部 ProviderService 的插件接口。
- 只新增一个窄范围、默认关闭、经过认证的本地控制 API。
- 本地同步小程序只负责从配置服务取配置并提交给 CC Switch，不解析或改写 Claude、Codex 等客户端配置文件。
- 模型级 Provider 路由、请求/响应 Hook 和交互审计都建立在现有代理链路上，不新增第二套反向代理。
- Hook 只提供稳定的调用协议和生命周期，不在 CC Switch 内置敏感信息规则或 Tool Call 安全策略；实际检测逻辑由用户层 Hook 实现。
- 完整交互记录使用独立的 `proxy_interactions` 数据模型和 Web 查看页，与现有用量统计表分离。

## 3. 为什么仍然需要本地代理

非代理模式下，CC Switch 会把供应商配置写入各 CLI 的 Live 配置文件。已运行的 CLI 不一定重新读取这些文件，通常需要重启才能稳定生效。

代理接管模式下，CLI 始终访问固定本地地址，例如：

```text
http://127.0.0.1:15721
```

CC Switch 在每次请求时选择当前 Provider，因此可以在不修改客户端连接地址、不重启 CLI 的情况下切换实际上游。

因此：

- 允许用户重启 CLI：可以不用代理，但仍需控制 API 或直接写配置。
- 要求自动、即时、无感切换：必须保留一个稳定代理入口；直接复用 CC Switch 即可。

## 4. 现有能力复用

必须复用以下现有路径，不直接操作 SQLite 或用户配置文件：

- `ProviderService::add`：创建受管 Provider。
- `ProviderService::update`：更新 URL、API Key、模型等配置。
- `ProviderService::switch`：按当前接管状态自动选择普通切换或代理热切换。
- `ProxyService`：代理启停、应用接管、Live 配置备份和恢复。
- `ProviderRouter`：按当前 Provider 路由每个请求。
- 现有 Deeplink Provider 配置构建逻辑：复用不同应用的通用 `endpoint + apiKey + model` 到内部 Provider 配置的转换，避免重新实现 Claude、Codex、Gemini、Grok Build 的字段映射。

首期支持现有 `supports_local_proxy()` 覆盖的应用：

- `claude`
- `codex`
- `gemini`
- `grokbuild`

## 5. MVP 范围

### 包含

1. CC Switch 中增加本地控制 API 开关和随机控制令牌。
2. 增加一个幂等的“应用远程路由配置”接口。
3. 使用固定 Provider ID 更新配置，避免重复创建供应商。
4. 本地同步小程序轮询配置服务并应用新版本。
5. 配置校验、鉴权、日志脱敏、失败重试和 last-known-good 行为。
6. 验证代理接管期间无需重启 CLI 即可完成切换。
7. 增加按应用和模型选择 Provider 的路由规则。
8. 增加默认关闭的请求/响应 Hook 框架。
9. 增加可脱敏、有限期的完整交互记录和本地 Web 查看页。

### 不包含

- 新的反向代理实现。
- 通用 CC Switch 插件 SDK。
- 暴露任意 Provider、数据库、MCP、Prompt 或 Skill 管理接口。
- 由控制 API 自动开启代理接管；首次开启仍由用户在 CC Switch UI 中明确完成。
- 多配置历史、灰度发布、复杂策略编排。
- API Key 仅驻留内存且绝不落盘的模式。
- CC Switch 内置敏感信息词库、通用 Prompt Injection 分类器或 Tool Call 风险判断器。
- 默认永久保存所有 Prompt、Response 和原始 SSE。

## 6. 控制 API 设计

### 6.1 开关与认证

控制 API 默认关闭。用户开启后，CC Switch 生成至少 32 字节随机令牌，供本地同步小程序使用。

所有控制请求必须同时满足：

- 请求来源是回环地址。
- 携带 `Authorization: Bearer <token>`。
- 使用 `Content-Type: application/json`。
- 拒绝带浏览器 `Origin` 的请求，避免网页直接调用本机控制面。

即使数据代理监听地址配置为 `0.0.0.0`，控制接口也不得接受非回环来源。现有手写 accept loop 已能取得客户端地址，实现时应把远端 `SocketAddr` 放入请求扩展供控制处理器校验。

令牌不得写入日志、URL、错误响应或代理请求记录。令牌轮换后旧令牌立即失效。

### 6.2 MVP 接口

只增加一个写接口：

```http
PUT /control/v1/routes/{app}
Authorization: Bearer <local-control-token>
Content-Type: application/json
```

请求示例：

```json
{
  "revision": "2026-09-21T10:30:00Z",
  "name": "Remote Managed",
  "endpoint": "https://api.example.com/v1",
  "apiKey": "secret",
  "model": "gpt-5.4"
}
```

字段约束：

| 字段 | 必填 | 说明 |
|---|---:|---|
| `revision` | 是 | 服务端配置版本，用于日志和响应，不作为密钥 |
| `name` | 否 | UI 显示名，默认 `Remote Managed` |
| `endpoint` | 是 | 实际上游 URL；默认只允许 HTTPS，回环开发地址可显式放行 HTTP |
| `apiKey` | 是 | 上游密钥，只能通过 JSON body 传递 |
| `model` | 否 | 供应商默认模型 |

成功响应：

```json
{
  "app": "codex",
  "providerId": "remote-managed",
  "revision": "2026-09-21T10:30:00Z",
  "active": true,
  "takeoverActive": true
}
```

错误状态：

| 状态码 | 场景 |
|---:|---|
| `400` | JSON、URL、应用类型或字段校验失败 |
| `401` | 缺少令牌或令牌错误 |
| `403` | 请求不是来自回环地址，或来自浏览器 Origin |
| `409` | 当前 Provider 类型不允许代理接管，例如不支持的官方认证供应商 |
| `503` | CC Switch 状态、数据库或代理服务不可用 |

### 6.3 应用配置语义

- 每个应用命名空间内使用固定 Provider ID：`remote-managed`。
- Provider 不存在时调用 `ProviderService::add`。
- Provider 已存在时调用 `ProviderService::update`。
- 更新成功后调用 `ProviderService::switch`；如果已经是当前 Provider，操作仍需幂等成功。
- 不直接调用 `Database::save_provider` 或 `set_current_provider`。
- 先完成完整输入校验和 Provider 构建，再修改现有状态。
- 如果更新或切换失败，返回错误并保留此前可用配置，不清空当前 Provider。

`revision` 的去重由本地同步小程序负责。CC Switch 接口保持幂等：重复提交相同配置不会创建新记录，也不会中断代理。

## 7. 本地同步小程序

### 7.1 形式

首期实现为单个无界面的后台程序，不做托盘 UI。配置项保持最少：

```text
remote_server_url
remote_access_token
cc_switch_url=http://127.0.0.1:15721
cc_switch_control_token
poll_interval_seconds=30
apps=claude,codex
```

后续只有在用户确实需要可视化状态、登录或手动切换时，再增加托盘界面。

### 7.2 同步流程

```text
启动
  │
  ├─ 检查 CC Switch /health
  │
  ├─ 携带远程凭据请求配置服务
  │    └─ 使用 ETag / If-None-Match，未变化则不下载
  │
  ├─ 校验 revision、app、endpoint、apiKey
  │
  ├─ 调用 PUT /control/v1/routes/{app}
  │
  ├─ 成功后保存 last_applied_revision
  │
  └─ 等待下一次轮询
```

行为要求：

- 同一时间只执行一次同步，不并发应用配置。
- 服务端返回空配置、非法配置或较旧版本时，不清除 last-known-good 配置。
- CC Switch 未启动或代理未接管时只记录脱敏错误并重试，不直接修改客户端配置。
- 网络错误使用有上限的指数退避；成功后恢复正常轮询间隔。
- 本地持久化只保存 `revision`、ETag 和非敏感状态，不重复保存 API Key。
- 日志中只记录应用、revision、HTTP 状态和脱敏后的 endpoint origin，不记录 API Key、Bearer Token 或完整响应体。

### 7.3 配置服务响应建议

```json
{
  "revision": "2026-09-21T10:30:00Z",
  "routes": {
    "claude": {
      "endpoint": "https://api.example.com",
      "apiKey": "secret",
      "model": "claude-sonnet-4-5"
    },
    "codex": {
      "endpoint": "https://api.example.com/v1",
      "apiKey": "secret",
      "model": "gpt-5.4"
    }
  },
  "modelRoutes": {
    "codex": {
      "rules": [
        {"model": "gpt-5.4", "providers": ["remote-managed"]},
        {"model": "o4-*", "providers": ["openrouter-b"]}
      ],
      "defaultProviders": ["remote-managed"]
    }
  }
}
```

配置服务必须使用 HTTPS。响应应支持 ETag，并始终返回完整当前配置，不下发需要本地合并的增量补丁。

## 8. 安全边界

### 信任模型

- 配置服务是受信任控制面；它可以决定实际上游并下发 API Key。
- 本地同步小程序与 CC Switch 运行在同一用户账户下。
- 本地控制令牌防止浏览器网页、其他本机进程和局域网客户端未经授权修改路由。
- CC Switch 继续负责 Provider 配置的本地持久化，因此 MVP 接受 API Key 落入现有 CC Switch 数据存储。

### 必须实现

- 远程配置只通过 HTTPS 获取。
- 本地控制接口只接受回环来源和 Bearer Token。
- 对 endpoint 做 scheme、host、长度和格式校验。
- 请求体设置较小上限，例如 64 KiB。
- 所有认证材料和配置响应默认脱敏。
- 不接受脚本、命令、任意文件路径或完整内部 Provider JSON。
- 不通过命令行参数或 Deeplink 传递 API Key。

如果未来要求 API Key 永不落盘，需要单独设计内存凭据存储和重启后的重新拉取流程，不纳入本 MVP。

## 9. 实施阶段

### 阶段一：CC Switch 控制 API

1. 增加控制 API 设置、令牌生成和轮换能力。
2. 在现有 Axum Router 中增加 `/control/v1/routes/{app}`。
3. 将 accept 得到的远端地址注入请求扩展。
4. 增加回环来源、Bearer Token、Origin 和 body 大小校验。
5. 将简化请求转换成现有 Provider 结构。
6. 通过 `ProviderService::add/update/switch` 应用配置。
7. 返回脱敏、稳定的 JSON 结果。

最小验证：

- 无令牌请求返回 `401`。
- 非回环请求返回 `403`。
- 非法 endpoint 不改变当前 Provider。
- 连续提交相同请求只保留一个 `remote-managed` Provider。
- 代理接管期间提交新 URL/Key 后，下一次请求走新上游且 CLI 无需重启。

### 阶段二：本地同步小程序

1. 实现配置文件读取和远程认证。
2. 实现带 ETag 的顺序轮询。
3. 校验服务端完整配置。
4. 调用 CC Switch 控制 API。
5. 保存 last-applied revision 和 ETag。
6. 增加退避、脱敏日志和优雅退出。

最小验证：

- 未变化配置不重复应用。
- 配置服务短暂不可用时继续保留现有路由。
- CC Switch 重启后同步程序能自动恢复提交。
- 非法配置不会覆盖 last-known-good。

### 阶段三：端到端与交付

1. 用两个可识别的测试上游 A/B 验证切换。
2. 保持 Claude/Codex CLI 会话运行，服务端将配置从 A 改为 B。
3. 确认一个轮询周期内后续请求进入 B。
4. 检查 CC Switch UI、`/status` 和请求日志显示的当前 Provider 一致。
5. 检查所有日志和错误响应不存在 API Key 与控制令牌。
6. 再决定是否需要开机启动、托盘 UI 和安装包集成。

## 10. 验收标准

- 用户只需首次启用 CC Switch 代理和应用接管。
- 服务端更新配置后，在一个轮询周期内自动生效。
- Claude/Codex 等客户端连接地址始终保持 localhost。
- 切换过程中不要求重启 CLI，不需要用户再次点击确认。
- 重复同步不会产生重复 Provider。
- 配置服务或 CC Switch 暂时不可用时，现有可用路由不被清除。
- 未认证或非本机请求无法调用控制 API。
- API Key 和令牌不出现在 URL、日志、错误响应和进程参数中。
- 实现没有新增代理、插件框架或直接数据库写入路径。

## 11. 后续条件触发项

仅在出现明确需求后再增加：

- 推送替代轮询：轮询延迟或服务压力成为实际问题时。
- 托盘 UI：用户需要查看同步状态或手动重新同步时。
- 多 Provider 策略：服务端需要下发故障转移队列而非单一当前路由时。
- 内存密钥：安全要求明确禁止 CC Switch 持久化 API Key 时。
- 独立控制端口或 Unix Socket / Named Pipe：同端口的回环校验和 Token 仍不能满足部署安全要求时。
- 服务端统一网关：希望完全移除本地实际供应商配置，并接受所有模型流量经过远程服务端时。

## 12. 扩展计划：模型级 Provider 路由

### 12.1 路由语义

现有 `ProviderRouter` 按应用选择当前 Provider；扩展后先按应用和客户端请求模型匹配规则，再在匹配到的 Provider 组内复用现有故障转移、熔断和半开恢复逻辑：

```text
应用 + 请求模型
       │
       ▼
模型路由规则
       │
       ▼
Provider 候选队列
       │
       ▼
现有 Provider 尝试 / 故障转移
```

规则必须明确以下优先级：

1. 精确模型名；
2. 用户定义的模型别名；
3. 通配符规则；
4. 应用默认 Provider 队列。

同一优先级出现多条匹配时拒绝保存，避免路由结果依赖配置顺序。没有匹配规则时保持现有应用级路由行为。匹配应使用客户端请求模型，Provider 的出站模型改写只在选定 Provider 之后发生。

### 12.2 控制 API

在现有控制 API 下增加幂等接口：

```http
PUT /control/v1/model-routes/{app}
Authorization: Bearer <local-control-token>
Content-Type: application/json
```

请求示例：

```json
{
  "revision": "2026-09-21T10:30:00Z",
  "rules": [
    {"model": "gpt-5.4", "providers": ["openai-a", "openrouter-b"]},
    {"model": "claude-*", "providers": ["anthropic-a"]}
  ],
  "defaultProviders": ["remote-managed"]
}
```

接口只修改路由规则，不接受任意 Provider JSON，也不绕过 `ProviderService`。Provider 不存在、模型规则冲突或候选队列为空时整份配置拒绝，保留 last-known-good 规则。

### 12.3 验收

- 同一应用的 `gpt-*` 和 `claude-*` 请求可进入不同 Provider。
- 模型未匹配时仍使用现有默认 Provider。
- 匹配到的候选 Provider 失败后，自动在该候选队列内切换。
- 路由日志记录匹配规则、客户端模型、出站模型和最终 Provider。

## 13. 扩展计划：请求与 Tool Call Hook 框架

### 13.1 生命周期

Hook 运行在本地代理请求生命周期内，最小接口分为三类：

```text
before_request      客户端请求已解析、尚未发往上游
after_response      上游响应已解析、尚未交给客户端
before_tool_call    已组装出完整 Tool Call、尚未交给客户端或执行器
```

Hook 输入只包含必要的结构化数据：`request_id`、应用、模型、Provider、消息/响应片段、Tool 名称和参数。Hook 输出使用固定结果：

```text
allow                 继续处理
block(reason)         阻断并返回安全错误
replace(payload)      使用替换后的脱敏内容继续
audit(event)          继续处理，但写入审计事件
```

Hook 由用户层提供实现，CC Switch 只负责调用、超时、大小限制、错误隔离和审计，不解释用户规则。首期只支持带独立 Bearer Token 的回环 HTTP Hook；目标地址必须是用户预先保存的固定回环地址，不能由单次 LLM 请求指定。不得允许 Hook 通过返回值携带新的 Provider 凭据或修改控制 API 权限。

### 13.2 敏感字符串检测

- 用户可以在 Hook 配置中提供敏感字符串列表、正则或外部检测逻辑。
- `before_request` 默认只允许 `allow`、`block`、`audit`；`replace` 必须显式开启。
- 命中后不得把原始敏感值写入日志、错误响应或 `proxy_interactions`。
- 检测失败的默认策略为继续请求并记录脱敏错误；安全要求高的用户可配置为阻断（fail-closed）。
- API Key、Bearer Token、Cookie 等代理自身认证材料无论 Hook 是否启用，都必须先经过内置脱敏器。

### 13.3 Tool Call 恶意行为检测

- `before_tool_call` 为用户 Hook 提供完整工具名、参数、模型和上下文摘要。
- CC Switch 不内置“恶意”判断，只执行 Hook 返回的 `allow/block/replace/audit`。
- 流式响应遇到 Tool Call 时，先在内存中聚合到完整调用，再交给 Hook；Hook 完成前不得把该调用片段发送给客户端。
- Hook 超时、崩溃或返回非法结果时按配置选择阻断或放行，并记录规则 ID 和请求 ID，不记录未脱敏参数。
- Tool Call 通过 Hook 后才进入客户端/执行器；CC Switch 不直接执行 Tool Call。

### 13.4 Hook 验收

- 自定义字符串命中时能阻断请求，且日志不包含原文。
- 自定义 Tool Hook 能阻断或替换危险参数。
- Hook 不可用时不会阻塞代理线程无限等待。
- 流式文本仍可透传；Tool Call 在 Hook 完成前不会提前泄漏。

## 14. 扩展计划：`proxy_interactions` 交互记录与 Web 应用

### 14.1 独立存储模型

现有 `proxy_request_logs` 继续只保存用量和状态。新增独立的 `proxy_interactions` 主记录及 `proxy_interaction_attempts` 尝试记录：

```text
proxy_interactions
- request_id
- session_id
- app_type
- client_model
- outbound_model
- final_provider_id
- status_code
- is_streaming
- request_payload_redacted
- response_payload_redacted 或 response_text
- created_at / completed_at
- retention_until

proxy_interaction_attempts
- request_id
- attempt_index
- provider_id
- endpoint_origin
- status_code
- error_code
- started_at / completed_at
```

`endpoint_origin` 只保存脱敏后的 origin；不保存 API Key。每次 Provider 尝试单独写入，才能还原 `A 失败 → B 成功` 的完整链路。记录失败不得影响向客户端返回响应，也不得阻塞正常用量统计。

### 14.2 记录策略

- 默认关闭完整 Body 记录；可按应用、模型或 Provider 开启。
- 开启后仍必须先脱敏 Authorization、API Key、Cookie 和用户配置的敏感字符串。
- 设置单请求大小上限、总磁盘配额和自动清理期限。
- 非流式请求保存脱敏 JSON；流式请求默认保存聚合后的文本和 Tool Call，原始 SSE 作为显式高级选项。
- 记录原始请求与上游转换后请求时，必须分开字段，避免误把客户端模型当成实际上游模型。
- UI 展示失败时只能显示元数据，不得因此暴露未脱敏内容。

### 14.3 Web 应用

首期复用 CC Switch 现有 Tauri WebView/前端，不另起 HTTP 服务。增加只读 Tauri command：

```text
list_proxy_interactions(filters, page)
get_proxy_interaction(request_id)
list_proxy_interaction_attempts(request_id)
```

查询支持时间、应用、模型、Provider、状态码、是否命中 Hook 和请求 ID 过滤。详情页至少展示：

- 客户端模型与出站模型；
- 最终 Provider；
- 全部 Provider 尝试时间线；
- 脱敏后的输入、输出和 Tool Call；
- Hook 事件、阻断原因和响应状态；
- Token、延迟和成本统计的关联入口。

这些 command 只注册给 CC Switch 自身窗口，不加入本地控制 API。默认列表不加载完整 Body，详情页需要二次确认，并显示记录的过期时间和脱敏状态。只有明确需要独立浏览器管理台时，才另行增加经过认证的只读 HTTP API。

### 14.4 交互记录验收

- 关闭记录时数据库不保存完整 Prompt/Response。
- 开启记录后可从详情页看到脱敏输入、输出和实际 Provider。
- 故障转移请求能看到每次 Provider 尝试及最终成功者。
- API Key、Bearer Token、自定义敏感字符串不会出现在数据库、接口响应或页面。
- 达到保留期限或配额后自动清理，不影响普通请求代理。

## 15. 综合实施顺序

1. 先完成模型路由规则和控制 API，复用现有 Provider 队列及故障转移。
2. 增加 Hook 调用边界和超时/脱敏/错误隔离，再接入用户自定义检测逻辑。
3. 增加 `proxy_interactions` 与逐 Provider attempt 记录，默认关闭内容保存。
4. 最后接入只读 Web 查询和详情页，验证权限、脱敏、保留期限和大响应性能。

完整交互记录、Hook 执行和 Tool Call 聚合都会增加延迟与隐私风险；只有在用户显式开启对应能力时才启用。模型路由和 Provider 故障转移则继续走现有代理路径，不复制一套新代理实现。
