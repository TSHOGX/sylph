# Issue #1 Step 2 修复计划

## 问题分类与优先级

### P0 - 数据完整性威胁（静默丢失）

1. **启动竞态** - bundle reader可能永久不启动
2. **resume重复投影** - 历史events重复进入journal
3. **sidecar metadata晚到** - `.meta.json`永久丢失
4. **nested抑制顺序依赖** - 子delegate被错误投影

### P1 - 架构偏离目标

5. **sidecar非真正按需** - 同步全量读取而非异步CAS
6. **同步Agent close缺失** - 只处理async launch

### P2 - 可观察性与健壮性

7. **raw record写入失败静默** - 只warn不阻断
8. **manifest version不持久** - 重启后碰撞
9. **native blob无CAS引用** - 无法sweep
10. **persistedOutputPath无校验** - 任意文件读取
11. **task-notification解析窄** - 可能漏掉未来位置

## 修复策略

### 阶段1：修复数据完整性威胁（P0）

#### 1.1 修复bundle_pump启动竞态

**根本原因**：`current` 状态在 `watch_live()` 成功之前就被更新

**解决方案**：
- 只在 `watch_live()` 成功后更新 `current`
- 失败时记录原因并在下次循环重试
- 添加结构化诊断：连续失败次数、最后错误

**设计原则**：状态变更必须与操作成功原子绑定

#### 1.2 修复resume时的baseline问题

**根本原因**：`BundleFollowState::new()` 不区分"首次读取"和"跟踪增量"

**解决方案**：
- 为live following引入 `ResumeBaseline` 概念
- 持久化每个source的已处理offset到服务端
- resume时从baseline继续而非从0开始

**设计原则**：live/reload必须有明确边界

#### 1.3 修复sidecar metadata晚到

**根本原因**：`.meta.json` 检查绑定在delegate首次打开逻辑中

**解决方案**：
- 为 `.meta.json` 建立独立的fingerprint cursor
- 每轮poll时独立扫描，不依赖delegate状态
- 发现变更时发送更新的native record

**设计原则**：每个source应该有独立的cursor和生命周期

#### 1.4 修复nested抑制的顺序依赖

**根本原因**：host tools收集和抑制决策在同一遍中，文件排序影响结果

**解决方案**：
- 引入两阶段poll：
  - Phase 1: 扫描所有sources，只收集metadata（host tools、workflow joins）
  - Phase 2: 基于完整metadata做投影决策
- 抑制决策在Phase 2开始前完全确定

**设计原则**：依赖关系需要拓扑排序，不能依赖遍历顺序

### 阶段2：修复架构偏离（P1）

#### 2.1 实现真正的按需sidecar读取

**根本原因**：`read_native_blob()` 同步、立即、完整读取

**解决方案**：
- 将sidecar改为CAS-addressed resource
- `NativeBlob` 只保存 `hash` 和 `size`，不保存 `bytes`
- 实现独立的异步ingestion管道
- 添加大小上限和明确的deferred状态

**设计原则**：大对象应该是可寻址的、异步的、有界的

#### 2.2 实现同步Agent close

**根本原因**：只处理async launch，没有处理同步完成

**解决方案**：
- 在 `project_record()` 中识别同步Agent的直接close
- 检查 `toolUseResult` 的 `agentId` + `isAsync: false` + `status: completed`
- 发送直接close event

**设计原则**：同步和异步路径都要完整支持

### 阶段3：增强可观察性（P2）

#### 3.1 raw record写入失败的结构化报告

- 引入 `NativeRecordLoss` counter/diagnostic
- 至少要有bounded retry或显式backpressure
- 考虑通过health endpoint暴露

#### 3.2 持久化manifest revisions

- 使用content hash而非进程内计数
- 或让数据库生成source-specific monotonic revision

#### 3.3 native blob的CAS引用

- 添加native CAS reference table
- 或让sweep同时读取 `native_blobs.hash`

#### 3.4 persistedOutputPath校验

- 限制必须在Claude native store根目录内
- 校验symlink/realpath

#### 3.5 扩展task-notification解析

- 对所有record做XML signal scan，不限于user/tool_result
- 保持结构性discriminant避免误判

## 实施顺序

1. **1.1** bundle_pump启动竞态 - 最严重，立即修复
2. **1.4** nested抑制顺序依赖 - 两阶段poll是后续基础
3. **1.3** sidecar metadata晚到 - 在两阶段框架中实现
4. **1.2** resume baseline - 需要schema变更
5. **2.1** 按需sidecar - 大重构，但是核心目标
6. **2.2** 同步Agent close - 独立feature
7. **3.x** 可观察性增强 - 渐进改进

## 测试策略

每个修复都需要对应的测试覆盖：

- **1.1**: 模拟transcript未创建时的 `watch_live()` 调用
- **1.2**: resumed session的历史baseline测试
- **1.3**: `.meta.json` 晚于delegate JSONL出现的测试
- **1.4**: nested delegate反向排序测试
- **2.1**: sidecar大小限制、异步读取测试
- **2.2**: 同步Agent close fixture
- **3.1**: raw persistence failure/retry测试
- **3.2**: server restart后manifest revision连续性测试
