# eomer-srt-ocr

用本地 llama.cpp 视觉模型（VLM）从影片画面中提取硬字幕，输出 SRT。

当前阶段：**Sprint 0 —— 可用的 CLI**。视频/图片输入、字幕带自动定位、OCR、弹幕/水印剔除、时间轴与 SRT 输出都已打通；Web 界面与多模型自动切换尚未开始。

## 快速开始

前置：

1. 本机有一个跑着 OCR 模型的 `llama-server`（需要加载 mmproj，多模态）。
   ```bat
   llama-server.exe -m D:\models\dots.ocr-Q8_0.gguf ^
     --mmproj D:\models\mmproj-dots.ocr-Q8_0.gguf ^
     --host 127.0.0.1 --port 8080 -ngl 99 -c 8192
   ```
2. 电脑上能跑 `ffmpeg` / `ffprobe`（会依次查找 `--ffmpeg`、`FFMPEG` 环境变量、PATH，最后是 `C:\Users\<user>\lib\ffmpeg-*\bin`）。

检查服务：

```bash
eomer-srt-ocr health                 # 默认 http://127.0.0.1:8080
```

视频：

```bash
eomer-srt-ocr video movie.mkv --out out/movie
# -> out/movie/output.srt     字幕文件
#    out/movie/frames.jsonl   每一帧的原始识别结果与耗时
#    out/movie/result.json    --json 时的运行摘要
```

图片：

```bash
eomer-srt-ocr image frame.jpg --out out/frame
# -> out/frame/frame.txt
```

模型清单（可选，用来记录模型参数与提示词）：

```bash
eomer-srt-ocr models --dir models
```

## 原理

```
ffmpeg 抽帧 ──► 字幕带定位 ──► 裁剪 ──► llama-server VLM ──► 文本后处理 ──► 时间轴 ──► SRT
                 (条带扫描)      (带+上下文)   (OpenAI 兼容)     (水印剔除)    (去重/切分)
```

几处非显然的设计：

- **字幕带靠模型判断，不靠像素启发式。** 画面里的亮窗户、白衬衫会让亮度统计完全失真。
  做法是把画面切成 8 条横向长条，用 VLM 分别 OCR，出现“短句、多行、可重复”特征的条
  就是字幕带。实测 30 帧里目标条命中 17 次，而水印条只有 1 次。
- **弹幕/水印在文本层剔除，不改图。** 水印是“固定文字、反复出现”，字幕是“每次不同”。
  实测 60 秒片段里水印占 34–52% 的帧，最长的一条字幕也只占 4%，两者界限非常清楚；
  再对近似的误读变体（`IP属地` / `IP屋地` / `IP 属地`）做一次距离合并即可清干净。
  这样不依赖水印的像素坐标，换片源也不用重新标定。
- **采样率自适应。** 时长 < 1 小时用 2 fps，更长用 1 fps，且不超过片源帧率。

## 已知限制

- 字幕跨条带时可能定位偏；当前用“上一条 + 下一条”的合并和上下留白缓解。
- 只有单条带检测，双行字幕占满整个下半屏的片源需要手调 `--probe-frames`。
- 逐帧调用 ffmpeg 进程，长片会明显变慢（下一步改成持续管道）。
- `--mask`（像素级遮挡）仍实验性，默认关闭。

## 测试

```bash
cargo test
```

- `tests/pure.rs` —— 纯逻辑：文本清理、编辑距离、时间轴合并/切分/去抖、SRT 格式、水印识别、fps 选择、掩码坐标。
- `tests/cli.rs` —— 进程契约：`--help`、非法参数、缺文件、死端口、模型清单加载与坏清单容错。

两者都不需要模型或网络。

## Windows 构建

```bat
cargo build --release
```

产物 `target\release\eomer-srt-ocr.exe`（单文件，依赖 VC++ 运行库）。
