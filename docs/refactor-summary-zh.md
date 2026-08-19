# Sylph Web 重构方案执行摘要

## 当前状态分析

**技术债务**：
- 纯 Node.js + 原生 ES 模块，无框架
- Vanilla JavaScript 前端（无 TypeScript、无 React）
- 直接使用 `node:sqlite` 访问数据库
- 约 500 行服务端 + 500 行客户端代码

**核心问题**：
- 缺乏类型安全
- 无现代前端框架支持
- 后端性能不足（JavaScript vs Rust）
- 可维护性差
- 测试覆盖率低

## 目标架构

### 技术栈

**前端**：
- **Next.js 16** (最新版) + App Router
- **React 19** (Server Components)
- **TypeScript 5.8+** (strict 模式)
- **Tailwind CSS 4.0+** (样式)
- **Radix UI** (无样式可访问组件)
- **TanStack Query 6.0+** (服务端状态管理)

**后端**：
- **Rust 1.85+** (性能、内存安全、类型安全)
- **Axum 0.8+** (Web 框架)
- **Tokio 1.44+** (异步运行时)
- **SQLx 0.8+** (编译时 SQL 检查)
- **Tower** (中间件)

### 系统架构

```
浏览器 (127.0.0.1:3000)
    │
    │ HTTP/SSE
    ▼
Next.js 16 前端
    │ API 代理
    ▼
Rust 后端 (127.0.0.1:4317)
    │
    ├─► live.db (SQLite) - 活跃会话
    ├─► TraceDB - 持久化历史
    └─► Runners (子进程) - 各种 coding agents
```

**核心特性**：
- **端到端类型安全**：TypeScript ↔ Rust 类型共享
- **流式更新**：SSE (Server-Sent Events) 实时推送事件
- **两层存储**：热数据在 live.db，冷数据在 TraceDB
- **进程隔离**：Rust 管理 runner 子进程，独立监控
- **本地优先**：127.0.0.1 绑定，cookie 认证，无云依赖

## 核心产品需求（保持不变）

### 双重会话模式

1. **Sylph 托管会话** (`sylph`)：
   - WebApp 创建
   - 注入 `SOUL.md`
   - 遵循 Sylph 生命周期策略（24h 超时、04:00 重置）

2. **原生会话** (`native`)：
   - 从其他 agent 的 TUI/CLI 导入
   - **绝不修改**：不注入 SOUL、不重置、不压缩、不改写提示词
   - 保持原始 agent 身份和生命周期

### 关键能力

- 通过 `--describe` 发现 coding agent runners
- 流式接收实时事件（NDJSON 协议）
- 渲染多种 agent 格式的标准化事件
- 通过 TraceDB 搜索跨 agent 的稳定历史
- 发送本地文件附件（最多 5 个，单个 8MB，总共 10MB）
- 执行 agent 特定命令（模型选择、状态检查等）

## 实施计划（10 周）

### Phase 1: 后端核心（第 1-3 周）

**目标**：
- Rust Axum 服务器（端口 4317）
- SQLite + SQLx + 迁移脚本
- 所有 `/api/v1/*` 端点
- Runner 发现和进程监控
- SSE 流式传输
- 认证中间件
- 集成测试套件

**关键路径**：
1. 数据库层（迁移、LiveStore）
2. Runner 注册表和监控器
3. API 处理器（sessions、turns、events）
4. SSE 实现
5. 测试

**交付标准**：
- 所有端点通过集成测试
- SSE 支持重连和回放
- 进程监控处理崩溃、超时、SIGKILL

### Phase 2: 前端基础（第 4-5 周）

**目标**：
- Next.js 16 App Router 结构
- API 代理路由
- 会话列表和对话 UI
- SSE 集成（EventSource）
- TanStack Query 设置

**关键路径**：
1. Next.js 设置和路由
2. 类型安全的 API 客户端
3. 会话列表组件
4. 对话时间线
5. SSE hook 和实时更新

**交付标准**：
- 可创建和查看会话
- 实时接收 turn 事件流
- Server Components 渲染静态内容
- Client Components 处理交互

### Phase 3: 功能对齐（第 6-7 周）

**目标**：
- 所有 UI 组件迁移
- 附件、命令、搜索
- 原生会话导入
- Agent 标志和过滤器
- 错误处理

**关键路径**：
1. 对话框（新建会话、命令）
2. 附件上传和验证
3. TraceDB 集成（搜索、导入）
4. 过滤器和标志
5. 错误边界

**交付标准**：
- 功能与当前版本对齐
- 支持附件上传
- 可搜索 TraceDB 历史
- 可执行 agent 命令

### Phase 4: 生产就绪（第 8-9 周）

**目标**：
- 全面测试覆盖
- 可访问性审计和修复
- 性能优化
- 文档
- 开发工具

**关键路径**：
1. 单元测试（后端 + 前端）
2. 集成测试（后端 API）
3. E2E 测试（Playwright）
4. 可访问性（键盘、屏幕阅读器）
5. 性能审计（bundle、SSE、DB 查询）

**交付标准**：
- 测试覆盖率 >80%
- WCAG 2.1 AA 合规
- LCP <2.5s, bundle <200KB
- 完整文档

### Phase 5: 部署（第 10 周）

**目标**：
- 打包二进制文件
- 迁移脚本
- 启动文档
- 回滚计划

**关键路径**：
1. 二进制打包（`cargo build --release`）
2. 前端生产构建
3. 数据迁移测试
4. 启动脚本
5. 监控设置

**交付标准**：
- 一键启动脚本
- 数据迁移经过测试
- LaunchAgent 配置（macOS）
- 健康检查和日志

## 预期收益

### 性能提升

- **后端延迟**：
  - 会话列表：10ms → <5ms（2x 提升）
  - Turn 创建：50ms → <30ms（1.7x 提升）
  - 内存占用：50MB → 20MB（2.5x 下降）

- **前端性能**：
  - LCP <2.5s（首次内容绘制）
  - Bundle <200KB（优化前端体积）
  - 虚拟滚动支持 >100 事件

### 开发体验

- **类型安全**：编译时捕获 90% 的类型错误
- **热重载**：前端（Turbopack）+ 后端（cargo-watch）
- **测试覆盖**：单元 + 集成 + E2E
- **代码质量**：eslint + prettier + clippy + rustfmt

### 可维护性

- **清晰边界**：前端（展示）、后端（逻辑）、数据（SQLite/TraceDB）
- **模块化架构**：易于添加新 agent、新功能
- **文档完善**：架构、API、组件、部署

## 风险和缓解

### 技术风险

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| Rust 学习曲线 | 中 | 高 | 从小模块开始，详尽文档，结对编程 |
| SSE 重连 Bug | 高 | 高 | 复用现有逻辑，E2E 测试，网络节流测试 |
| 进程监控边缘案例 | 中 | 高 | 测试崩溃、SIGKILL、孤儿进程清理 |
| 数据迁移失败 | 低 | 严重 | 在副本上测试，事务性迁移，回滚计划 |

### 产品风险

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| 功能遗漏 | 中 | 中 | 详细的功能对齐清单，回归测试 |
| UX 倒退 | 低 | 中 | 参考 DeepSeek Harness，用户测试 |
| 性能倒退 | 中 | 中 | 早期性能分析，基准测试，优化热路径 |

## 下一步行动

1. **确认架构**：审查完整规范文档（`docs/refactor-specs.md`）
2. **设置环境**：Rust、pnpm、开发工具
3. **启动 Phase 1**：创建 Rust 项目，数据库层，基础 API
4. **持续集成**：每个 phase 结束进行集成测试和演示

## 相关文档

- **完整规范**：`docs/refactor-specs.md` (2092 行)
- **当前架构**：`docs/architecture.md`
- **产品设计**：`docs/product.md`
- **API 规范**：`docs/api-v1.openapi.json`