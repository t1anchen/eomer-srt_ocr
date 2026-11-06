// Minimal front end: no build step, no framework. The server embeds this file
// at compile time, so the binary stays self-contained.

import { openVideo, defaultBand, timestamps } from './frames.js';

const VIDEO_EXT = ['mp4', 'mkv', 'mov', 'webm', 'avi', 'flv', 'ts', 'm4v'];
const IMAGE_EXT = ['png', 'jpg', 'jpeg', 'webp', 'bmp'];
const POLL_MS = 1000;

const jobsEl = document.getElementById('jobs');
const dropEl = document.getElementById('drop');
const fileEl = document.getElementById('file');
const backendEl = document.getElementById('backend');

// job id -> DOM nodes we update in place, so polling never rebuilds the list.
const rendered = new Map();

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
  if (kind === 'video') {
    // Try streaming first. Only a container the browser cannot decode falls
    // through to a whole-file upload for the server's ffmpeg.
    if (await streamVideo(file)) return;
  }
  try {
    const r = await fetch(`/api/v0/upload/${kind}`, {
      method: 'POST',
      headers: { 'X-Filename': file.name, 'Content-Type': 'application/octet-stream' },
      body: file,
    });
    const j = await r.json();
    if (!r.ok) throw new Error(j.error || r.statusText);
    upsert(j);
  } catch (e) {
    alert(`上传失败：${e.message}`);
  }
}

function upsert(view) {
  let nodes = rendered.get(view.id);
  if (!nodes) {
    const card = document.createElement('article');
    card.className = 'job';
    card.innerHTML = `
      <div class="job-head">
        <span class="job-name"></span>
        <span class="job-state"></span>
      </div>
      <div class="bar"><i style="width:0%"></i></div>
      <div class="stage"></div>
      <div class="error" hidden></div>
      <div class="result"></div>`;
    jobsEl.prepend(card);
    nodes = {
      card,
      name: card.querySelector('.job-name'),
      state: card.querySelector('.job-state'),
      bar: card.querySelector('.bar > i'),
      stage: card.querySelector('.stage'),
      error: card.querySelector('.error'),
      result: card.querySelector('.result'),
      painted: false,
    };
    rendered.set(view.id, nodes);
  }
  nodes.name.textContent = view.original_name;
  nodes.name.title = view.original_name;
  nodes.state.textContent = view.state;
  nodes.state.className = `job-state ${view.state}`;
  const pct = view.fraction == null ? 0 : Math.round(view.fraction * 100);
  nodes.bar.style.width = `${pct}%`;
  nodes.stage.textContent = view.stage;

  if (view.error) {
    nodes.error.hidden = false;
    nodes.error.textContent = view.error;
  } else {
    nodes.error.hidden = true;
  }

  // The result block is expensive to build; do it once per job.
  if (view.state === 'done' && !nodes.painted) {
    nodes.painted = true;
    paintResult(nodes.result, view);
  }
}

function paintResult(host, view) {
  const o = view.outcome;
  if (!o) return;
  host.textContent = '';

  if (o.kind === 'image') {
    const pre = document.createElement('div');
    pre.className = 'out-text';
    pre.textContent = o.text || '(没有识别到文字)';
    host.appendChild(pre);
    return;
  }

  const list = document.createElement('ul');
  list.className = 'cues';
  for (const c of o.cues) {
    const li = document.createElement('li');
    const t = document.createElement('time');
    t.textContent = `${fmtTime(c.start)} → ${fmtTime(c.end)}`;
    const span = document.createElement('span');
    span.textContent = c.lines.join(' / ');
    li.append(t, span);
    list.appendChild(li);
  }
  host.appendChild(list);

  const actions = document.createElement('div');
  actions.className = 'actions';
  const srt = document.createElement('a');
  srt.className = 'btn';
  srt.href = `/api/v0/jobs/${view.id}/srt?disposition=attachment`;
  srt.textContent = '下载 SRT';
  const open = document.createElement('a');
  open.className = 'btn';
  open.href = `/api/v0/jobs/${view.id}/srt`;
  open.target = '_blank';
  open.textContent = '查看 SRT';
  actions.append(srt, open);
  host.appendChild(actions);

  const summary = document.createElement('div');
  summary.className = 'stage';
  summary.textContent =
    `${o.cues.length} 条字幕 · ${o.frames_with_text}/${o.frames_processed} 帧有字 · ` +
    `${o.elapsed_secs.toFixed(1)} 秒`;
  host.appendChild(summary);
}

async function poll() {
  try {
    // Legacy jobs use the v0 view shape. v1 streams are rendered by newCard()
    // while their frames are uploaded; feeding them into upsert() would create
    // a second, blank card because StreamView has no job outcome fields.
    const r = await fetch('/api/v0/jobs');
    if (r.ok) {
      const list = await r.json();
      for (const v of list) upsert(v);
    }
  } catch (e) {
    // A transient failure is not worth interrupting the user for; the next
    // tick retries.
  }
  setTimeout(poll, POLL_MS);
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
    return false; // browser cannot decode it; the server's ffmpeg can
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
    return true; // it was ours; do not retry as a whole-file upload
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
