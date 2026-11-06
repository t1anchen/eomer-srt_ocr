// Minimal front end: no build step, no framework. The server embeds this file
// at compile time, so the binary stays self-contained.

import { openVideo, defaultBand, timestamps } from './frames.js';

const VIDEO_EXT = ['mp4', 'mkv', 'mov', 'webm', 'avi', 'flv', 'ts', 'm4v'];
const IMAGE_EXT = ['png', 'jpg', 'jpeg', 'webp', 'bmp'];

const jobsEl = document.getElementById('jobs');
const dropEl = document.getElementById('drop');
const fileEl = document.getElementById('file');
const backendEl = document.getElementById('backend');

function extOf(name) {
  const i = name.lastIndexOf('.');
  return i < 0 ? '' : name.slice(i + 1).toLowerCase();
}

function kindOf(name) {
  const e = extOf(name);
  if (VIDEO_EXT.includes(e)) return 'video';
  if (IMAGE_EXT.includes(e)) return 'image';
  return null;
}

function fmtTime(sec) {
  const s = Math.max(0, sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const pad = (n, w = 2) => String(n).padStart(w, '0');
  return `${pad(h)}:${pad(m)}:${pad(r.toFixed(3), 6)}`;
}

async function checkBackend() {
  try {
    const r = await fetch('/api/v1/health');
    const j = await r.json();
    if (j.backend_reachable) {
      backendEl.textContent = `后端就绪 · ${j.backend}`;
      backendEl.className = 'ok';
    } else {
      backendEl.textContent = `后端未启动 · ${j.backend}`;
      backendEl.className = 'bad';
    }
  } catch (e) {
    backendEl.textContent = '无法连接服务';
    backendEl.className = 'bad';
  }
}

async function upload(file) {
  const kind = kindOf(file.name);
  if (!kind) {
    alert(`不支持的文件类型：${file.name}`);
    return;
  }
  if (kind !== 'video') {
    alert('图片任务需要在命令行运行：eomer-srt-ocr image <file>');
    return;
  }
  // The video path is local decode plus frame streaming. There is deliberately
  // no whole-file fallback: a container the browser cannot decode is reported
  // instead of being uploaded for the server to buffer.
  await streamVideo(file);
}

// --- drag and drop ---------------------------------------------------------
dropEl.addEventListener('dragover', (e) => {
  e.preventDefault();
  dropEl.classList.add('over');
});
dropEl.addEventListener('dragleave', () => dropEl.classList.remove('over'));
dropEl.addEventListener('drop', (e) => {
  e.preventDefault();
  dropEl.classList.remove('over');
  for (const f of e.dataTransfer.files) upload(f);
});
fileEl.addEventListener('change', () => {
  for (const f of fileEl.files) upload(f);
  fileEl.value = '';
});

checkBackend();
setInterval(checkBackend, 5000);
poll();

// --- frame streaming -------------------------------------------------------

// Decode locally and post frames one at a time. Returns true when the video
// went all the way through; false means the caller should fall back to
// uploading the file and letting the server run ffmpeg.
async function streamVideo(file) {
  const card = newCard(file.name);
  const setStage = (t) => (card.stage.textContent = t);
  const setBar = (f) => (card.bar.style.width = `${Math.round(f * 100)}%`);

  let src;
  try {
    setStage('正在解码视频…');
    src = await openVideo(file);
  } catch (e) {
    card.card.remove();
    alert('浏览器无法解码这个视频容器，请先转码为 H.264 的 MP4 或 WebM');
    return false;
  }

  try {
    const band = defaultBand(src.height);
    const open = await fetch('/api/v1/streams', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        fps: src.fps,
        width: src.width,
        height: src.height,
        band,
        cropped: true,
      }),
    });
    const stream = await open.json();
    if (!open.ok) throw new Error(stream.error || open.statusText);
    const id = stream.id;

    const times = timestamps(src.fps, src.duration);
    card.state.textContent = 'running';
    card.state.className = 'job-state running';

    for (let i = 0; i < times.length; i++) {
      const blob = await src.frame(times[i], band);
      // One retry per frame: a dropped connection should cost a frame, not a job.
      let ok = false;
      for (let attempt = 0; attempt < 2 && !ok; attempt++) {
        try {
          const r = await fetch(`/api/v1/streams/${id}/frames?index=${i}`, {
            method: 'POST',
            headers: { 'Content-Type': 'image/png' },
            body: blob,
          });
          if (!r.ok) {
            const j = await r.json().catch(() => ({}));
            throw new Error(j.error || r.statusText);
          }
          ok = true;
        } catch (e) {
          if (attempt === 1) throw e;
        }
      }
      setBar((i + 1) / times.length);
      setStage(`识别中 ${i + 1}/${times.length} 帧`);
    }

    setStage('整理时间轴…');
    const fin = await fetch(`/api/v1/streams/${id}/finish`, { method: 'POST' });
    const view = await fin.json();
    if (!fin.ok) throw new Error(view.error || fin.statusText);

    card.state.textContent = 'done';
    card.state.className = 'job-state done';
    card.stage.textContent =
      `${view.cues} 条字幕 · ${view.frames_with_text}/${view.frames_received} 帧有字 · ` +
      `${view.elapsed_secs.toFixed(1)} 秒`;
    card.bar.style.width = '100%';

    const a = document.createElement('a');
    a.className = 'btn';
    a.href = `/api/v1/streams/${id}/srt?disposition=attachment`;
    a.textContent = '下载 SRT';
    card.card.appendChild(a);
    return true;
  } catch (e) {
    card.state.textContent = 'failed';
    card.state.className = 'job-state failed';
    card.error.hidden = false;
    card.error.textContent = e.message;
    return false; // already reported on the card; nothing left to fall back to
  } finally {
    src.close();
  }
}

// A card not tied to a server job id, used while frames are streaming.
function newCard(name) {
  const card = document.createElement('article');
  card.className = 'job';
  card.innerHTML = `
    <div class="job-head"><span class="job-name"></span><span class="job-state"></span></div>
    <div class="bar"><i style="width:0%"></i></div>
    <div class="stage"></div>
    <div class="error" hidden></div>`;
  card.querySelector('.job-name').textContent = name;
  jobsEl.prepend(card);
  return {
    card,
    state: card.querySelector('.job-state'),
    bar: card.querySelector('.bar > i'),
    stage: card.querySelector('.stage'),
    error: card.querySelector('.error'),
  };
}
