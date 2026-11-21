"""Small audio toolkit for sound design in numpy: decode any file through
ffmpeg to float32 stereo at 48 kHz, write WAV, measure loudness over time,
spectral balance, onsets."""
import subprocess
import wave

import numpy as np

SR = 48000


def load(path, sr=SR, mono=False):
    """Any media file -> float32 array (n, 2) (or (n,) when mono) at sr."""
    ch = 1 if mono else 2
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-ac", str(ch), "-ar", str(sr), "-"],
                         capture_output=True, check=True).stdout
    a = np.frombuffer(raw, dtype=np.float32).copy()
    return a if mono else a.reshape(-1, 2)


def save(path, x, sr=SR):
    x = np.asarray(x, dtype=np.float64)
    if x.ndim == 1:
        x = np.stack([x, x], axis=1)
    pcm = (np.clip(x, -1, 1) * 32767).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm.tobytes())


def mono(x):
    return x.mean(axis=1) if x.ndim == 2 else x


def rms_db(x, win, sr=SR):
    """RMS in dBFS over consecutive windows of win seconds."""
    m = mono(x)
    n = int(win * sr)
    k = len(m) // n
    if k == 0:
        return np.array([20 * np.log10(np.sqrt(np.mean(m ** 2)) + 1e-9)])
    r = np.sqrt(np.mean(m[: k * n].reshape(k, n) ** 2, axis=1))
    return 20 * np.log10(r + 1e-9)


def bands(x, sr=SR):
    """Share of energy below 150 Hz, 150 Hz - 2 kHz, above 2 kHz; spectral centroid."""
    m = mono(x)
    if len(m) < 2048:
        m = np.pad(m, (0, 2048 - len(m)))
    S = np.abs(np.fft.rfft(m * np.hanning(len(m)))) ** 2
    f = np.fft.rfftfreq(len(m), 1 / sr)
    tot = S.sum() + 1e-20
    lo, mid, hi = S[f < 150].sum() / tot, S[(f >= 150) & (f < 2000)].sum() / tot, S[f >= 2000].sum() / tot
    cen = (S * f).sum() / tot
    return lo, mid, hi, cen


def peak_time(x, sr=SR):
    """Time of the loudest 10 ms window: where a hit lands."""
    m = np.abs(mono(x))
    n = int(0.01 * sr)
    k = max(1, len(m) // n)
    e = m[: k * n].reshape(k, n).max(axis=1)
    return float(np.argmax(e) * n / sr)


def onset_time(x, sr=SR, thresh_db=-30):
    """First time the envelope rises within thresh_db of the peak."""
    m = np.abs(mono(x))
    n = int(0.005 * sr)
    k = max(1, len(m) // n)
    e = m[: k * n].reshape(k, n).max(axis=1)
    lim = e.max() * 10 ** (thresh_db / 20)
    return float(np.argmax(e >= lim) * n / sr)


def spectrogram_png(x, path, sr=SR, width=1400, height=360, fmin=30, fmax=16000, marks=(), label=""):
    """A log-frequency spectrogram image (time left to right), with vertical
    marks at given seconds: to see a mix's structure."""
    from PIL import Image, ImageDraw
    m = mono(x).astype(np.float64)
    hop = max(1, len(m) // width)
    nfft = 4096
    pad = np.pad(m, (nfft // 2, nfft // 2))
    win = np.hanning(nfft)
    f = np.fft.rfftfreq(nfft, 1 / sr)
    rows = np.exp(np.linspace(np.log(fmax), np.log(fmin), height))
    idx = np.clip(np.searchsorted(f, rows), 0, len(f) - 1)
    img = np.zeros((height, width))
    for c in range(width):
        s = c * hop
        seg = pad[s:s + nfft]
        if len(seg) < nfft:
            break
        S = np.abs(np.fft.rfft(seg * win))
        img[:, c] = 20 * np.log10(S[idx] + 1e-9)
    img = np.clip((img + 20) / 90, 0, 1)
    rgb = (np.stack([img ** 0.8, img ** 1.2, img ** 2.2], axis=2) * 255).astype(np.uint8)
    im = Image.fromarray(rgb)
    d = ImageDraw.Draw(im)
    dur = len(m) / sr
    for t in marks:
        xx = int(t / dur * width)
        d.line([(xx, 0), (xx, height)], fill=(80, 160, 255), width=1)
    for hz in (100, 1000, 10000):
        y = int(np.interp(np.log(hz), [np.log(fmin), np.log(fmax)], [height, 0]))
        d.line([(0, y), (12, y)], fill=(255, 255, 255))
        d.text((14, y - 6), f"{hz}", fill=(255, 255, 255))
    if label:
        d.text((40, 4), label, fill=(255, 255, 255))
    im.save(path)
    return path
