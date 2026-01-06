Rust MCP Proxy 设计文档（面向 Augment Context Engine / auggie 后端）
目标与背景

当前使用 auggie --mcp（Node 运行时）存在问题：

多个 Node 进程持续累积，占满 CPU（尤其是多个 workspace/project 时）。

IDE 退出后 Node 进程残留，需要频繁重启电脑。

即使只开一个项目，仍可能出现“未启用的 MCP 也在后台跑”的现象（重复配置、自动发现、多实例）。

本项目实现一个 Rust 编写的 MCP 代理（Proxy），作为 IDE 唯一连接的 MCP server：

对 IDE：只看到一个 MCP server（单实例）。

对后端：按 workspace root 维持 0..N 个 auggie 后端实例（按需启动、空闲回收、LRU 限流）。

对进程管理：代理退出时 强制杀死全部子进程（Windows Job Object Kill-on-close），杜绝残留。

对性能：通过降低实例数、节流、忽略目录、空闲回收显著降低 CPU/内存压力。

注意：本方案不重写 Augment 云 API，也不做逆向。仅做 MCP 协议转发 + 生命周期治理。

非目标（Non-goals）

不实现/替代云端检索/embedding/上下文服务逻辑。

不改变 auggie/context-engine 的内部算法，仅作为进程与请求路由层。

不保证完全消除索引 CPU（扫描/监听本身仍存在），重点是 避免重复实例 + 残留。

需求与约束
功能需求

MCP server（stdio）：代理通过 stdin/stdout 与 IDE 通信（JSON-RPC）。

后端管理：按 workspace root 启动 auggie 后端实例：

每个 root 至多 1 个后端实例（去重）。

支持并发多个 root（可配置最大数 K）。

请求转发：将 IDE 请求按 root 路由到对应后端，返回结果给 IDE。

安全退出：代理进程退出时，所有 child node/auggie 进程都被系统强制终止。

空闲回收：后端闲置超过 T 分钟自动关停（可配置）。

日志与诊断：输出关键生命周期事件到 stderr/日志文件：

backend spawn / ready / shutdown

route decision（哪个 root -> 哪个 backend）

request id 映射（debug 级别）

spawn 失败原因、重试策略

关键约束

IDE/客户端通常会按 MCP 规范调用 initialize，并可能传递 roots 或通过后续通知更新 roots。

代理需要兼容“roots 不提供/只提供一个”的情况（退化模式）。

Windows 重点：需要防止 .cmd -> cmd.exe -> node.exe 脱管链路导致残留。

总体架构
           ┌─────────────────────────┐
IDE <->    │  Rust MCP Proxy (stdio) │
JSON-RPC   │  - Router               │
           │  - Backend Pool         │
           │  - ID Mapper            │
           │  - Job Object (win)     │
           └──────────┬──────────────┘
                      │
          ┌───────────┴───────────┐
          │                       │
  ┌───────▼────────┐     ┌────────▼───────┐
  │ auggie backend  │ ... │ auggie backend │
  │ (node, per root)│     │ (node, per root)│
  └─────────────────┘     └────────────────┘

核心模块设计
1) Stdio JSON-RPC Server（MCP Frontend）

职责：

读取 stdin 的 JSON-RPC 消息（单行 JSON 或按 Content-Length framing，依实际 MCP 实现）。

解析为 JsonRpcRequest/Notification。

对 initialize/shutdown/exit 做必要的协议处理。

将可路由请求交给 Router，等待响应并写回 stdout。

建议：

使用 tokio 异步 IO。

统一错误返回格式（JSON-RPC error）。

2) Router（路由决策）

职责：

将每个请求映射到一个 workspace root（root key）。

获取/创建该 root 对应的 backend 句柄。

将请求交给 backend bridge 并等待响应。

root 决策策略（按优先级）：

若 MCP 提供 roots（initialize 或 roots changed 通知），根据请求上下文/URI 路由：

如果请求携带 uri（如 file://...），找其所属 root（最长前缀匹配）。

若无 uri/roots：

使用“默认 root”（启动参数传入或首次 initialize 记录的 root）。

仍无法确定：

路由到 fallback backend（单实例模式），并记录 warning。

3) Backend Pool（后端池）

数据结构：

HashMap<RootKey, BackendInstance>

LRU 结构记录最近使用时间（或用 last_used: Instant + 定时清理）

可配置：

MAX_BACKENDS = K（超出则淘汰 LRU）

IDLE_TTL = T 分钟（超时关闭）

SPAWN_TIMEOUT / READY_TIMEOUT

BackendInstance 内容：

child process handle（tokio process）

stdin/stdout pipes

状态机：Spawning -> Ready -> Stopping -> Dead

last_used

一个独立 task：读取 backend stdout，解析 JSON-RPC response，回填到 ID Mapper 的 pending map

淘汰策略：

超过 MAX_BACKENDS：先关停 LRU 且非活跃（无 pending 请求）者

空闲超过 IDLE_TTL：关停

若有 pending 请求则延后淘汰，或设置 hard timeout

4) ID Mapper（多后端请求 id 映射）

问题：

IDE 发来的 JSON-RPC id 在多个 backend 并发时可能冲突。

若原样转发，backend A/B 都可能用同一个 id，代理无法区分返回。

解决：

代理生成 proxy_id（全局递增 u64 或 uuid）。

建立映射：

proxy_id -> (client_id, root_key, backend_id)

backend_id -> proxy_id（可选）

转发流程：

收到 client request（client_id 可能是 number/string）：

分配 proxy_id（u64），并在 pending map 注册 oneshot sender。

组装发给 backend 的 request：把 id 替换成 proxy_id（或 backend_id）。

后端返回 response：取 id 找到 pending，得到对应的 client_id，并把 response 的 id 改回 client_id，再写回 IDE。

注意：

JSON-RPC id 允许 string/number/null，需完整保留 client_id 类型。

notification（无 id）不进入 pending map，直接转发。

5) Windows 进程治理（Job Object）

目标：

代理退出时，所有 node/auggie 子进程必须被杀死（包括其子孙进程）。

实现要点：

Windows 下创建 Job Object，并设置 JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE。

每次 spawn backend child 后，将 child process handle AssignProcessToJobObject。

代理退出时关闭 Job handle => OS 自动杀死 job 中全部进程树。

备注：

若 child 已在其他 job 中（某些限制），需要 fallback 策略：显式 kill process tree（如 taskkill /T /F 或 Windows API）。

非 Windows：使用进程组（Unix setsid）并 killpg。

后端启动方式（重要）
推荐：绕过 .cmd，直接启动 node + js

避免 auggie.cmd 引入 cmd.exe 造成“脱管”。

启动配置：

node.exe（绝对路径）

auggie 的 JS entry（从 npm 安装目录定位）

参数：--mcp -m default --workspace-root <root>

示例（概念）：

node.exe <auggie_entry.js> --mcp -m default --workspace-root E:\...\pochi


代理应支持配置：NODE_PATH, AUGGIE_ENTRY, AUGGIE_ARGS_TEMPLATE。

配置设计（CLI / 环境变量）

建议命令行：

mcp-proxy.exe ^
  --backend "auggie" ^
  --node "C:\Program Files\nodejs\node.exe" ^
  --auggie-entry "C:\Users\...\node_modules\...\auggie\...\entry.js" ^
  --mode default ^
  --max-backends 3 ^
  --idle-ttl-seconds 600 ^
  --log-level info


环境变量（可选）：

AUGMENT_DISABLE_AUTO_UPDATE=1（减少额外动作）

MCP_PROXY_LOG=debug

生命周期与状态机
MCP 生命周期

initialize：

记录 client capabilities、roots（若提供）

返回 server capabilities（尽量透传 backend 的 capabilities 或固定声明）

正常请求：

Router -> BackendPool -> BackendBridge

shutdown：

标记 shutting down

停止接收新请求（或返回 error）

优雅关闭 backend（发送 shutdown/exit 或直接 kill，按实现阶段）

exit：

立即退出（触发 job object kill）

Backend 生命周期

Spawn：

创建进程

绑定 Job Object

等待 backend ready（可通过读到 initialize response 或健康探针）

Ready：

接收转发请求

Idle：

超时自动 shutdown

Crash：

标记 Dead，pending 请求返回错误或重试（配置）

错误处理与重试策略

Spawn 失败：记录错误，返回 JSON-RPC error 给 IDE。

Backend 崩溃：对 pending 请求返回错误（或最多重试 1 次重新拉起）。

超时：请求超时返回错误，并可标记 backend unhealthy。

解析错误：记录 raw 行，避免代理崩溃。

建议定义错误码：

-32001 backend_spawn_failed

-32002 backend_unavailable

-32003 backend_timeout

-32004 routing_failed

性能策略（CPU 降压）

单实例优先：能共享 roots 就只跑一个代理进程。

限制后端数量：MAX_BACKENDS 默认 2~3。

空闲回收：IDLE_TTL 默认 10 分钟。

节流：

对文件变更相关请求做 debounce（如果 MCP 层有此类通知）。

忽略目录：

建议用户在 workspace 设置 .augmentignore 排除 node_modules/ dist/ build/ .git/ ...

代理也可在启动后端时注入环境变量或参数（如果后端支持）。

安全与隐私

代理不解析/保存代码内容，仅转发。

日志默认不打印完整请求 body（避免泄露代码），仅打印方法名、root、耗时。

Debug 模式下才打印部分 payload，并建议脱敏。

验收标准（Acceptance Criteria）

打开/关闭 IDE 多次后：

不再出现 Node 进程残留（任务管理器中 node.exe 数量回落到 0 或稳定）。

同时打开多个项目：

进程数不再线性增长；后端数量 <= MAX_BACKENDS。

CPU 峰值可接受：

初次索引有峰值，但稳定后显著低于当前“常年 100%”。

代理崩溃/退出：

所有子进程被 OS 清理（Job Object 生效）。

迭代计划（分阶段交付）
Phase 0：最小可用（MVP）

实现 stdio JSON-RPC 读写

单 backend 转发（不做多 root）

Windows Job Object kill-on-close（强制无残留）

基础日志

Phase 1：多 root & 后端池

root 路由（uri 前缀匹配）

多 backend 管理（HashMap）

ID Mapper（proxy_id）

idle TTL + LRU 淘汰

Phase 2：健壮性与性能

backpressure（限制并发请求数）

backend crash 自动重启（有限次数）

更完整的 MCP capabilities 透传/协商

更完善日志与诊断命令（例如 status）

测试方案

单元测试：

ID Mapper：并发请求/响应映射正确

Router：uri->root 匹配

集成测试：

启动代理 + mock backend（回显 JSON-RPC）

多 backend 并发、LRU 淘汰

Windows 手工验证：

强制关闭 IDE/kill 代理 => node 子进程是否立即全部消失

反复开关 IDE 多次，检查进程是否累积

风险与备选方案

风险：IDE 不支持单实例 roots（每 workspace 都会拉一个 server）

备选：仍可用代理，但只能做到“每 workspace 一个代理”，收益主要在 Job Object 进程清理与后端节流。

风险：auggie 后端协议/行为不稳定

备选：代理加健康检查与自动重启策略；或退化为启动器方案（仅做进程治理）。

附录：实现建议（Rust 技术栈）

异步运行时：tokio

JSON：serde_json

进程：tokio::process::Command

Windows Job Object：windows-sys 或 windows crate 调用 WinAPI

LRU：lru crate 或自实现（HashMap + VecDeque/LinkedHashMap）