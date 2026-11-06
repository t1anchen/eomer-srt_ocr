# 在异步处理函数里构造阻塞式 HTTP 客户端

- 日期：2026-W45-4
- 状态：已修复，v0 路径整体删除

## 现象

向 v0 上传接口 POST 图片：

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

## 根因

`reqwest::blocking::Client` 内部持有一个单线程 runtime，其创建与销毁只能在普通线程上进行。
`async fn upload` 里同步调用了 `LlamaClient::new()`，于是该 runtime 在 tokio worker
线程（异步上下文）里被创建，随后又在异步上下文中被销毁，`BlockingPool::shutdown` 直接 panic。

崩溃位置：`upload` 处理函数，位于当时的 `src/server/routes.rs`。

## 修复

v1 逐帧接口已完全覆盖该场景，v0 整包上传路径连同其四条路由整体删除，
前端在浏览器无法解码时改为明确提示。详见 TODO「v0 整包上传…工作线程崩溃」。
