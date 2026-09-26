# Linux 服务端 ACP 重连排查

浏览器连接恢复与 ACP 智能体重启是两件事。短暂隧道断流应只恢复
WebSocket，再携带原连接 ID、租约和消息游标接入；服务进程退出则会
关闭 ACP 子进程，无法靠浏览器保活维持正在执行的任务。

## 本次修复

- 浏览器发送现有的 30 秒保活时，检查 15 秒内是否收到 `pong`；
  无回应时进入现有的健康检查及退避重连流程。
- 页面回到前台或网络恢复时，立即探测现有连接；后台页面的心跳超时
  不直接判定故障，回到前台后重新给予回应时间。
- WebSocket 建连及应用握手超过 15 秒时重试，避免永久停在连接中。
- 重复启动服务时先检查并占用监听端口，端口冲突即退出，不再先执行
  数据库恢复、任务恢复和后台清理。

## 部署时需要处理的事项

1. 同时部署本次构建的 `codeg-server` 和 `web/` 静态资源，保留现有
   数据目录、访问令牌及智能体配置。重启服务会中断运行中的任务，
   应在任务结束后进行。
2. 检查 `ps -eo pid,ppid,lstart,args` 和 `ss -ltnp 'sport = :3080'`，
   确认只有一个启动链管理同一服务。若使用 `--supervise`，父监督进程
   和一个工作进程属于正常结构；不要再由另一套 watchdog 独立启动工作进程。
3. 结合 systemd journal、容器事件或 watchdog 日志核查重启来源。
   `shutdown signal received` / `signal terminated` 表示收到终止信号，
   单凭应用日志不能确定发送者。`Address already in use` 表示重复启动
   或其他程序占用了端口，不应通过反复停止健康服务来解决。
4. 健康检测应检查服务的 HTTP 响应；仅 TCP 端口可连接不代表初始化完成。
   初始化期间应留出启动宽限，不要因一次探测失败就停止进程。

## 验证

- 短暂中断隧道后恢复，确认原会话继续显示输出，没有创建新的智能体进程。
- 页面切到后台再返回，确认能恢复输出；浏览器控制台若出现
  `[WebTransport] recovering WebSocket`，其原因会标明 `heartbeat_timeout`、
  `handshake_timeout`、`send_failed` 或 `socket_error`。
- 再启动一个使用相同监听地址的服务，应立即报端口冲突退出，现有会话不受干扰。

不要仅为隐藏重连提示而关闭租约清理。默认客户端租约为 90 秒，普通共享会话
空闲回收为 900 秒；`CODEG_ACP_IDLE_TIMEOUT_SECS=0` 也不会关闭租约到期及
临时会话清理。临时连接、探测连接和已完成子任务的正常清理不等于当前会话故障。

## 租约心跳调试日志

这些日志只说明 30 秒 `{action:"ping"}` 有没有发出，以及服务端有没有续约。
租约时长、续约结果和回收策略不变。

浏览器 DevTools Console 过滤 `[lease-heartbeat]`，并把级别调到 Verbose。
`console.debug` 在默认的 Info 视图里不显示。

- `[WebEventStream][lease-heartbeat] start` / `stop` / `not started`：
  共享与非共享订阅数量、缩短后的 connection / subscription id，以及是否带有
  generation 和 leaseId。没有 `shared` 的观察者或委托子会话不会启动心跳。
- `[WebEventStream][lease-heartbeat] tick sent` / `tick skipped`：
  跳过原因是 `ws not open`、`destroyed`、`no shared subs` 或 `send failed`。
- `[WebTransport][lease-heartbeat] wake probe sent` / `wake probe skipped`：
  页面回到前台或 `online` 时的探测。跳过原因是 `hidden`、`not connected`、
  `ws closed`、`destroyed` 或 `send failed`。

服务端默认级别是 info，不会写出续约摘要。行在 stderr 和
`codeg-server.<date>.log` 里。`CODEG_LOG` 优先于 `RUST_LOG`；只设置其中一个：

```bash
RUST_LOG=codeg_lib::web::ws=debug
```

示例（不含 lease id）：

```text
[WS][lease] ping had nothing to renew subscriptions=2 unbound=2
[WS][lease] ping bindings=1 renewed=1 detached=0 subscriptions=2 unbound=1 outcomes=[connection=<id> generation=4 renewed]
[WS][lease] ping bindings=1 renewed=0 detached=1 subscriptions=1 unbound=0 outcomes=[connection=<id> generation=4 detached:lease_expired]
```
