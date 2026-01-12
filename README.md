# MCP Proxy for Augment Context Engine

Rust 实现的 MCP 代理，用于管理 Auggie 后端实例的生命周期，解决进程残留和 CPU 占用问题。

**支持平台**: Windows / macOS (Intel & Apple Silicon) / Linux

## 功能特性

- **跨平台支持**: 支持 Windows、macOS（Intel/Apple Silicon）和 Linux
- **单实例锁**: 全局锁确保只有一个 proxy 实例运行（Windows: Mutex, Unix: flock）
- **多 workspace 支持**: 按需为不同 workspace root 启动后端
- **进程治理**: 退出时自动清理所有子进程（Windows: Job Object, Unix: ProcessGroup）
- **资源管理**: LRU 淘汰 + 空闲回收，限制后端数量
- **事件节流**: 文件变更通知合并去重，防止 CPU 风暴
- **Git 过滤**: 只处理 git 跟踪的文件，自动排除 node_modules
- **资源限制**: 支持设置后端进程优先级（macOS 不支持 CPU 亲和性）
- **配置文件**: 支持 JSON 配置文件，简化部署
- **自动检测**: 自动检测 Node.js 和 Auggie 安装路径

## 安装

### 前置要求

1. 安装 [Node.js](https://nodejs.org/)
2. 安装 Auggie: `npm install -g @augmentcode/auggie`

### 编译

```bash
cargo build --release
```

生成的可执行文件位于：
- Windows: `target/release/mcp-proxy.exe`
- macOS: `target/release/mcp-proxy`

### 预编译二进制

从 [GitHub Releases](../../releases) 下载对应平台的预编译文件：
- `mcp-proxy.exe` - Windows x64
- `mcp-proxy-macos-x64` - macOS Intel
- `mcp-proxy-macos-arm64` - macOS Apple Silicon
- `mcp-proxy-linux-x64` - Linux x64

## 快速开始

### 最简配置

MCP 配置（Windsurf / VS Code）：

**Windows:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "args": ["--default-root", "E:\\your-project"],
      "command": "path/to/mcp-proxy.exe"
    }
  }
}
```

**macOS:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "args": ["--default-root", "/Users/yourname/your-project"],
      "command": "/path/to/mcp-proxy-macos-arm64"
    }
  }
}
```

**Linux:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "args": ["--default-root", "/home/yourname/your-project"],
      "command": "/path/to/mcp-proxy-linux-x64"
    }
  }
}
```

程序会自动检测 Node.js 和 Auggie 路径。

### 带 Augment 登录环境变量

如果需要配置 Augment API 认证：

**Windows:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "command": "path/to/mcp-proxy.exe",
      "args": ["--default-root", "E:\\your-project"],
      "env": {
        "AUGMENT_API_TOKEN": "your-access-token",
        "AUGMENT_API_URL": "your-tenant-url"
      }
    }
  }
}
```

**macOS:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "command": "/path/to/mcp-proxy-macos-arm64",
      "args": ["--default-root", "/Users/yourname/your-project"],
      "env": {
        "AUGMENT_API_TOKEN": "your-access-token",
        "AUGMENT_API_URL": "your-tenant-url"
      }
    }
  }
}
```

**Linux:**
```json
{
  "mcpServers": {
    "augment-context-engine": {
      "command": "/path/to/mcp-proxy-linux-x64",
      "args": ["--default-root", "/home/yourname/your-project"],
      "env": {
        "AUGMENT_API_TOKEN": "your-access-token",
        "AUGMENT_API_URL": "your-tenant-url"
      }
    }
  }
}
```

### 使用配置文件

创建 `mcp-proxy.json`（放在 exe 同目录）：

```json
{
  "default_root": "E:\\your-project",
  "git_filter": true,
  "debounce_ms": 500
}
```

MCP 配置简化为：

```json
{
  "mcpServers": {
    "augment-context-engine": {
      "command": "path/to/mcp-proxy.exe"
    }
  }
}
```

## 配置参数

### 命令行参数

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--node` | `MCP_PROXY_NODE_PATH` | 自动检测 | node.exe 路径 |
| `--auggie-entry` | `MCP_PROXY_AUGGIE_ENTRY` | 自动检测 | auggie 入口文件路径 |
| `--default-root` | `MCP_PROXY_DEFAULT_ROOT` | - | 默认 workspace root |
| `--mode` | - | `default` | auggie 模式 |
| `--max-backends` | - | `3` | 最大后端实例数 |
| `--idle-ttl-seconds` | - | `600` | 空闲超时（秒） |
| `--log-level` | `MCP_PROXY_LOG` | `info` | 日志级别 |
| `--debounce-ms` | - | `500` | 事件节流窗口（毫秒） |
| `--git-filter` | - | `false` | 只处理 git 跟踪的文件 |
| `--low-priority` | - | `true` | 设置后端为低优先级 |
| `--cpu-affinity` | - | `0` | CPU 亲和性掩码 |

### 配置文件

配置文件搜索顺序：

**Windows:**
1. exe 同目录 `mcp-proxy.json`
2. 当前工作目录 `mcp-proxy.json`
3. `%USERPROFILE%\.config\mcp-proxy.json`
4. `%USERPROFILE%\mcp-proxy.json`

**macOS/Linux:**
1. 可执行文件同目录 `mcp-proxy.json`
2. 当前工作目录 `mcp-proxy.json`
3. `~/.config/mcp-proxy.json`
4. `~/.mcp-proxy.json`

配置优先级：**命令行参数 > 环境变量 > 配置文件 > 自动检测**

### 完整配置文件示例

```json
{
  "node": "C:\\Program Files\\nodejs\\node.exe",
  "auggie_entry": "C:\\Users\\xxx\\AppData\\Roaming\\npm\\node_modules\\@augmentcode\\auggie\\augment.mjs",
  "default_root": "E:\\my-project",
  "mode": "default",
  "max_backends": 3,
  "idle_ttl_seconds": 600,
  "log_level": "info",
  "debounce_ms": 500,
  "git_filter": true,
  "low_priority": true,
  "cpu_affinity": 0
}
```

## 架构

```
IDE <─stdio─> MCP Proxy <───> Backend Pool
                  │               │
                  │               ├── auggie (workspace A)
                  │               ├── auggie (workspace B)
                  │               └── auggie (workspace C)
                  │
                  ├── 单实例锁 (Windows: Mutex / Unix: flock)
                  ├── 事件节流器
                  ├── Git 文件过滤
                  └── 进程清理 (Windows: Job Object / Unix: ProcessGroup)
```

## 性能优化建议

### Node.js 项目

启用 Git 过滤，自动排除 `node_modules`：

```json
{
  "git_filter": true,
  "debounce_ms": 1000
}
```

### 低配电脑

限制 CPU 使用：

```json
{
  "cpu_affinity": 3,
  "low_priority": true,
  "max_backends": 1
}
```

`cpu_affinity: 3` = 0x03 = 只使用 CPU 核心 0 和 1

## License

MIT
