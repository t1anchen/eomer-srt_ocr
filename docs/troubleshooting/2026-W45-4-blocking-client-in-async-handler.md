# 在异步处理函数里构造阻塞式 HTTP 客户端

- 日期：2026-W45-4
- 状态：v1 已修复并实测通过；v0 路径仍带病，见 TODO「v0 整包上传在处理函数里构造阻塞式客户端导致工作线程崩溃」
- 影响范围：`--features http` 构建的 v0 上传接口

## 现象

向 v0 上传接口 POST 一个 329 KB 的 PNG：

```
HTTP 000
```

请求没有拿到任何响应，连接直接被切断。进程本身没有退出——

```
thread 'tokio-rt-worker' panicked at tokio-1.54.1/src/runtime/blocking/shutdown.rs:51:21:
Cannot drop a runtime in a context where blocking is not allowed.
This happens when a runtime is dropped from within an asynchronous context.
```

一个 worker 线程塌了，其余线程照常处理别的请求，所以表面上服务还在。这类故障不会让进程崩掉，只会让一部分请求静默失败。

## 根因

`reqwest::blocking::Client` 内部持有一个单线程 runtime。这个 runtime 的创建和销毁都只能在普通线程上进行。

`async fn upload` 里同步调用了 `LlamaClient::new()`，它内部构造 `reqwest::blocking::Client::new()`。于是那个 runtime 是在 tokio worker 线程（异步上下文）里建起来的，随后在 async 上下文中被销毁，`BlockingPool::shutdown` 直接 panic。

backtrace 把链路钉死在一行上：

```
reqwest::blocking::wait::enter          wait.rs:80
ClientHandle::new                       client.rs:1417
ClientBuilder::build                    client.rs:116
LlamaClient::new                        src/ocr.rs:66
upload::{closure#0}                     src/server/routes.rs:272   ← 就是这里
```

## 归因过程中走过的弯路

抓 backtrace 之前，我做了两轮错误归因，都建立在推演上而不是证据上：

**第一轮：怪销毁路径。** 以为 `LlamaClient` 作为 job 结构体成员被 drop 时触发了 panic。曾在独立进程里验证过 job worker 在线程上 drop 是安全的，验证结论本身没错，但它证明的不是故障点。

**第二轮：怪 runtime 顺序。** 以为是 blocking 客户端先于 tokio runtime 构造导致的顺序问题，并且只给 `GET /api/health` 的探针包了 `spawn_blocking`。真正出问题的路径一行没动。

两轮都失败之后才开 `RUST_BACKTRACE=1` 抓栈，一次定位。

**教训**：`Cannot drop a runtime in a context where blocking is not allowed` 这条消息只说明「runtime 被销毁的位置不对」，完全没说「runtime 在哪里被创建」。真正的创建点才是要修的地方，而它只出现在 backtrace 里。开着 backtrace 复现一次，比推演三轮便宜得多。

## 修复

不是修补 v0，而是换掉整条上传模型。

v0 的做法是「整包上传 → 服务端抽帧 → 服务端裁带 → 服务端 OCR」。新的 v1 改成流式：

```
POST /api/v1/streams                      开流，服务端回 band 定义
POST /api/v1/streams/{id}/frames?index=N  一次一帧（已裁好的字幕带 PNG）
POST /api/v1/streams/{id}/finish          建时间轴、写盘、幂等
GET  /api/v1/streams/{id}/srt             取 SRT
```

关键点：

- **客户端裁剪**，每帧约 120 KB（640x150 PNG），不是整包视频
- **在 handler 里不碰阻塞式客户端**，OCR 调用都在普通线程上
- 单帧带一次重试，断线只丢一帧

实测（640 宽、5 秒片段、15 帧）：

```
0:200 1:200 ... 14:200
{"state":"done","frames_received":15,"frames_with_text":3,"cues":2,"elapsed_secs":2.00}
```

## 连带发现的三个 v0 缺陷

同一条路径上还有三处问题，一并记在这里，都没修：

1. **请求体上限不一致**：`MAX_UPLOAD_BYTES` 声明 4 GiB，axum 默认 body limit 只有 2 MiB，大文件在上传阶段就被框架拒掉，返回的错误信息也不是我们写的
2. **整包读进内存**：`Bytes` 把整个文件驻留内存，4 GiB 上限下必然 OOM
3. **任务索引线性扫描**：按 id 查 job 是遍历整个列表

v1 的流式模型从设计上回避了前两条，第三条在 `StreamStore` 里用 `HashMap` 解决。

## 顺带纠正的一处设计错误

v1 第一版跑出来 10 帧全 200 但 `frames_with_text: 0`。

原因是**二次裁剪**：客户端按 band 裁好了 150px 高的字幕带，服务端又把它当成 360px 全帧再裁一次，把字幕裁掉了。

修复是为流加 `cropped` 标志，客户端声明「我传的是裁好的图」后服务端不再裁。这是接口契约漏了一种合法输入，不是实现 bug。

## 复现要点

- 必须开 `RUST_BACKTRACE=1`，否则 panic 消息只说「context 不对」，看不到构造点
- 用 `--features http` 构建才有 serve 子命令
- 单独复现：Post 任意图片到 v0 上传接口即可
