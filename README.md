# Lark Docs Bot RS

基于 **Rust / Axum** 的飞书 Docx 文档网关：浏览器编辑器、REST API 与 MCP（无状态 Streamable HTTP JSON 响应）。

## 当前能力

- 浏览器：文档创建、列表、原文/结构化预览，标题、列表、引用的新增与编辑；多 Text Run 分片编辑、加粗/斜体工具栏、待提交队列、编辑冲突提示。
- REST：文档及 Block 读写、分页、元数据、指定版本删除、访问控制。
- MCP：`initialize`、`ping`、`tools/list`、`tools/call`；工具包括 `list_documents`、`create_document`、`read_document`、`list_blocks`、`append_text`、`append_structured`、`update_text_block`、`update_text_runs`、`delete_block`。
- 安全：Bearer 管理员/写入密钥、网关文档登记白名单、匿名只读显式授权、单实例角色限流、文本 Run 原文校验、防止混合嵌入块误覆盖。
- 运维：Docker / Compose、Caddy HTTPS + CSP、容器健康检查、持久化文档登记数据、备份与恢复手册、CI 及 GHCR 镜像发布。

## 本地运行

```bash
cp .env.example .env
# 编辑 .env，填写 LARK_APP_ID / LARK_APP_SECRET，并生成两份不同的 32+ 字符 API Key
docker compose up -d --build
curl -fsS http://127.0.0.1:3000/health
```

Feishu App 权限与应用发布状态必须支持 Docx API；只将凭证保存在私有的环境文件或托管服务秘密管理中。GitHub Actions secrets `APPID` 和 `APPSECRET` 仅用于手动的飞书真实联调，**不会自动配置生产主机**。

## REST 接口

| 方法 | 路径 | 授权 |
| --- | --- | --- |
| GET | `/health` | 无 |
| GET | `/`、`/assets/app.js`、`/assets/app.css` | 无 |
| GET/POST | `/api/docs` | 读：写入者；创建：管理员 |
| GET | `/api/docs/{id}` | 写入者，或文档显式匿名只读 |
| GET | `/api/docs/{id}/blocks?page_token=...` | 同上 |
| GET | `/api/docs/{id}/metadata` | 同上 |
| GET | `/api/docs/{id}/blocks/{parent_id}/children` | 同上 |
| POST | `/api/docs/{id}/append` | 写入者 |
| POST | `/api/docs/{id}/append-structured` | 写入者 |
| PATCH | `/api/docs/{id}/blocks/{block_id}` | 写入者 |
| PATCH | `/api/docs/{id}/blocks/{block_id}/runs` | 写入者 |
| POST | `/api/docs/{id}/blocks/delete` | 写入者，须指定版本和父块位置 |
| POST | `/api/docs/{id}/access` | 管理员 |
| POST | `/mcp` | Bearer API Key |

所有受保护请求发送 `Authorization: Bearer <API_KEY>`。`update_text_runs` 按原有 Run 顺序校验 `expected_text`，保留未主动修改的样式和其他属性。请求可按 Run 显式调整 `bold`/`italic`；不支持编辑嵌入式图片、表格和复杂非文本元素。读取后校验**不是原子的**：极端并发仍需由 Feishu 原生协作机制处理。

### 富文本请求示例

```json
{"runs":[{"expected_text":"Plain ","text":"Updated ","bold":false},{"expected_text":"bold","text":"strong","italic":true}]}
```

提交到 `PATCH /api/docs/{id}/blocks/{block_id}/runs`。对重要业务文档使用前，应运行手动 Feishu `test_multirun` 真实联调。

## MCP 协议

`/mcp` 提供无状态 HTTP JSON-RPC；目前支持 `2025-03-26`、`2025-06-18` 与 `2025-11-25` 协商，POST 请求返回 JSON，通知返回 HTTP 202，GET/DELETE 返回 HTTP 405。服务器不发出 MCP Session ID，也不提供服务器主动推送 SSE、会话恢复或 MRTR。适用于支持**无状态 JSON 响应**的 MCP HTTP 客户端，不宣称支持所有流式扩展。除 `initialize` 外，建议客户端发送所选择的 `MCP-Protocol-Version` 和 `Accept: application/json, text/event-stream`。

完整的工具发现与请求协议可通过 `bash scripts/mcp-smoke.sh` 自动检查，运行前需要 `cargo build --bin lark-docs-bot-rs`。

## 测试与发布

- 常规 CI：`cargo fmt`、`clippy`、`cargo test`、Release 构建、MCP HTTP 测试、Web JS 语法/安全 DOM 测试、生产 Compose 配置验证、Docker 构建。
- [飞书真实联调](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/feishu-smoke.yml)：手动运行、使用当次新建的测试文档；可勾选 `test_delete`、`test_structured`、`test_multirun`，默认全部关闭。
- [OCI 镜像发布](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/publish.yml)：发布到 GHCR。当前发布工作流输出 `linux/amd64`；ARM 宿主机可使用本仓库 Dockerfile 本机编译。
- [生产发布](https://github.com/hu-qi/lark-docs-bot-rs/actions/workflows/deploy.yml)：手动确认，需部署主机 SSH secrets + 可选 GitHub Environment 保护。**未配置服务器时不会自动上线**。

## 生产部署

阅读 [deploy/PRODUCTION.md](deploy/PRODUCTION.md)。简要步骤：配置 DNS 和可持久化 Docker Linux 宿主机，将 `deploy/env.production.example` 复制为根目录 `.env.production` 并填写真实秘密；运行 `bash deploy/preflight.sh`；然后通过 GitHub Actions 的 `Deploy to production host` 手动触发部署。工作流使用对应 Commit 的 GHCR 镜像，将应用、Caddy 与备份工具镜像传输至服务器，执行 `docker compose --env-file .env.production -f compose.prod.yaml up -d --no-build`；ECS 无须本地编译 Rust。Caddy 自动申请 HTTPS 证书，应用端口不会公开。定期运行 `bash deploy/backup.sh /absolute/path/to/backups` 并将加密备份异地保存。

生产部署要求：主机防火墙、独立 IP 频率限制或 WAF、监控与报警、离线可恢复备份、集中安全日志和实际 TLS 验收。应用内限流是**单进程、按角色**，不是分布式/按 IP 限流；登记文件也是单实例持久化 JSON，而非多实例数据库。

## 已知限制

- 编辑器仍不是与飞书完全等价的复杂富文本编辑器；跨 Run 光标范围、图片/表格/嵌入对象等需转到飞书原生编辑器。
- 浏览器默认读取最多 10 页（约 1,000 个 Block），超出需额外分页；嵌套 Block 未递归展开。
- Web 样式预览覆盖常用文字加粗/斜体、标题和列表，不保证飞书完整排版像素级一致。
- 网关 `public_read` 仅影响本服务，不修改飞书原生分享权限。
- 实际上线、公开 DNS、服务器健康与生产读写测试需要可访问的容器宿主机及凭证，仓库 CI 成功不等于部署成功。

**安全提醒**：如应用密钥曾在聊天、截图或日志中泄露，应尽快轮换，并更新私有 CI Secrets 与生产主机配置。
