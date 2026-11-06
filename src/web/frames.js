// Browser-side frame extraction: decode the video locally, crop the subtitle
// band, and hand back one frame at a time. The server never sees the video
// file, only small crops, so a two-hour movie costs no upload at all.
//
// Browsers only decode containers they already support (H.264 in MP4/WebM).
// For anything else the UI falls back to whole-file upload, which the server
// hands to ffmpeg.

const SAMPLE_SHORT_SIDE = 360;

// Pick a sampling rate from duration. Short clips get a dense grid so a blink
// of dialogue is not missed; long ones trade resolution for runtime.
export function chooseFps(durationSecs) {
  if (!(durationSecs > 0)) return 2;
  if (durationSecs <= 120) return 2;
  if (durationSecs <= 1800) return 1;
  if (durationSecs <= 7200) return 0.5;
  return 0.25;
}

// Subtitle band guess for a frame of `height`. The pipeline discovers the real
// band from pixel statistics; this is the opening hand for the first frames,
// and the server refines it once enough frames arrive.
export function defaultBand(height) {
  return { y0: Math.round(height * 0.66), y1: height };
}

// Seek a video element and wait for the frame to be paintable.
function seek(video, t) {
  return new Promise((resolve, reject) => {
    const done = () => {
      video.removeEventListener('seeked', done);
      video.removeEventListener('error', fail);
      resolve();
    };
    const fail = () => {
      video.removeEventListener('seeked', done);
      video.removeEventListener('error', fail);
      reject(new Error(`cannot decode frame at ${t.toFixed(2)}s`));
    };
    video.addEventListener('seeked', done, { once: true });
    video.addEventListener('error', fail, { once: true });
    video.currentTime = t;
  });
}

function toBlob(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error('canvas encoding failed'))),
      'image/png'
    );
  });
}

// Open a video file, expose its geometry, and return a frame reader.
export async function openVideo(file) {
  const url = URL.createObjectURL(file);
  const video = document.createElement('video');
  video.preload = 'auto';
  video.muted = true;
  video.playsInline = true;
  video.src = url;

  await new Promise((resolve, reject) => {
    video.addEventListener('loadedmetadata', resolve, { once: true });
    video.addEventListener('error', () => reject(new Error('this browser cannot decode this file')), {
      once: true,
    });
  });

  const vw = video.videoWidth;
  const vh = video.videoHeight;
  const duration = video.duration;
  if (!vw || !vh) {
    URL.revokeObjectURL(url);
    throw new Error('the file has no video stream');
  }

  // Scale so the short side is at least SAMPLE_SHORT_SIDE; the OCR model
  // downsamples anyway, and smaller crops OCR more accurately than full frames.
  const scale = Math.max(SAMPLE_SHORT_SIDE / Math.min(vw, vh), 1);
  const width = Math.round(vw * scale);
  const height = Math.round(vh * scale);

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d', { willReadFrequently: false });

  return {
    width,
    height,
    duration,
    fps: chooseFps(duration),

    // Render frame at timestamp `t`, cropped to `band` (in output pixels).
    async frame(t, band) {
      await seek(video, Math.min(t, Math.max(0, duration - 1e-3)));
      ctx.drawImage(video, 0, 0, width, height);
      if (!band || (band.y0 === 0 && band.y1 === height)) {
        return toBlob(canvas);
      }
      const y0 = Math.max(0, Math.min(band.y0, height - 1));
      const y1 = Math.max(y0 + 1, Math.min(band.y1, height));
      const crop = document.createElement('canvas');
      crop.width = width;
      crop.height = y1 - y0;
      crop.getContext('2d').drawImage(canvas, 0, y0, width, y1 - y0, 0, 0, width, y1 - y0);
      return toBlob(crop);
    },

    async image() {
      ctx.drawImage(video, 0, 0, width, height);
      return toBlob(canvas);
    },

    close() {
      video.removeAttribute('src');
      video.load();
      URL.revokeObjectURL(url);
    },
  };
}

// Sample timestamps for `fps` over a duration.
export function timestamps(fps, duration) {
  const step = 1 / fps;
  const out = [];
  for (let t = 0; t < duration; t += step) out.push(Math.round(t * 1000) / 1000);
  return out;
}
