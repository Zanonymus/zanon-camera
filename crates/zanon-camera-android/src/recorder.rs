//! Video recording: raw preview YUV -> MediaCodec H.264 (and AAudio -> AAC), muxed by our own MP4
//! writer. The platform MediaRecorder/MediaMuxer is never used, so no creation time, location or
//! vendor atoms are written.

use std::fs::File;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ndk::audio::{AudioDirection, AudioFormat, AudioPerformanceMode, AudioStreamBuilder};
use ndk::media::media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection};
use ndk::media::media_format::MediaFormat;
use crate::jni_util::log;
use zanon_camera_core::capture::Nv12;
use zanon_camera_core::mp4::{split_annexb, AudioConfig, Mp4Writer, VideoConfig};

const KEY_FRAME: u32 = 1;
const CONFIG: u32 = 2;
const EOS: u32 = 4;

struct Mux {
    w: Option<Mp4Writer<File>>,
    video: Option<usize>,
    audio: Option<usize>,
}

type Shared = Arc<Mutex<Mux>>;

pub struct Recorder {
    venc: MediaCodec,
    shared: Shared,
    stop_audio: Arc<AtomicBool>,
    audio_thread: Option<std::thread::JoinHandle<()>>,
    first_ts_ns: Option<i64>,
}

fn drain(codec: &MediaCodec, shared: &Shared, is_video: bool, wait: Duration) -> bool {
    loop {
        match codec.dequeue_output_buffer(wait) {
            Ok(DequeuedOutputBufferInfoResult::Buffer(buf)) => {
                let (flags, pts, size, off) = {
                    let i = buf.info();
                    (i.flags(), i.presentation_time_us(), i.size() as usize, i.offset() as usize)
                };
                if flags & CONFIG == 0 && size > 0 {
                    let mut m = shared.lock().unwrap();
                    let track = if is_video { m.video } else { m.audio };
                    if let (Some(t), Some(w)) = (track, m.w.as_mut()) {
                        let _ = w.write_sample(t, &buf.buffer()[off..off + size], pts, !is_video || flags & KEY_FRAME != 0);
                    }
                }
                let _ = codec.release_output_buffer(buf, false);
                if flags & EOS != 0 {
                    return true;
                }
            }
            Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                let f = codec.output_format();
                log(&format!("format changed video={is_video}"));
                let mut m = shared.lock().unwrap();
                if is_video {
                    let (Some(c0), Some(c1)) = (f.buffer("csd-0"), f.buffer("csd-1")) else { continue };
                    let (sps, pps) = (split_annexb(c0).first().map(|n| n.to_vec()), split_annexb(c1).first().map(|n| n.to_vec()));
                    if let (Some(sps), Some(pps), Some(w)) = (sps, pps, m.w.as_mut()) {
                        let cfg = VideoConfig { width: f.i32("width").unwrap_or(0) as u16, height: f.i32("height").unwrap_or(0) as u16, sps, pps };
                        m.video = Some(w.add_video(cfg));
                    }
                } else if let (Some(asc), Some(w)) = (f.buffer("csd-0").map(|b| b.to_vec()), m.w.as_mut()) {
                    let cfg = AudioConfig {
                        sample_rate: f.i32("sample-rate").unwrap_or(44100) as u32,
                        channels: f.i32("channel-count").unwrap_or(1) as u16,
                        asc,
                    };
                    m.audio = Some(w.add_audio(cfg));
                }
            }
            Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
            Ok(DequeuedOutputBufferInfoResult::TryAgainLater) | Err(_) => return false,
        }
    }
}

fn audio_loop(shared: Shared, stop: Arc<AtomicBool>) -> Option<()> {
    let stream = AudioStreamBuilder::new()
        .ok()?
        .direction(AudioDirection::Input)
        .format(AudioFormat::PCM_I16)
        .channel_count(1)
        .sample_rate(44100)
        .performance_mode(AudioPerformanceMode::LowLatency)
        .open_stream()
        .map_err(|e| log(&format!("audio open: {e:?}")))
        .ok()?;
    let rate = stream.sample_rate();
    let mut f = MediaFormat::new();
    f.set_str("mime", "audio/mp4a-latm");
    f.set_i32("sample-rate", rate);
    f.set_i32("channel-count", 1);
    f.set_i32("bitrate", 96_000);
    f.set_i32("aac-profile", 2);
    f.set_i32("max-input-size", 16384);
    let enc = MediaCodec::from_encoder_type("audio/mp4a-latm")?;
    enc.configure(&f, None, MediaCodecDirection::Encoder).map_err(|e| log(&format!("aac configure: {e:?}"))).ok()?;
    enc.start().map_err(|e| log(&format!("aac start: {e:?}"))).ok()?;
    stream.request_start().map_err(|e| log(&format!("audio start: {e:?}"))).ok()?;
    log(&format!("audio started at {rate} Hz"));
    let mut pcm = vec![0i16; 1024];
    let mut frames: u64 = 0;
    let timeout = Duration::from_millis(50);
    loop {
        let done = stop.load(Ordering::SeqCst);
        let n = match unsafe { stream.read(pcm.as_mut_ptr().cast(), 1024, 50_000_000) } {
            Ok(n) => n as usize,
            // ndk 0.9 reports a successful read (positive frame count) as an unknown error
            Err(ndk::audio::AudioError::__Unknown(n)) if n > 0 => n as usize,
            Err(e) => {
                log(&format!("audio read: {e:?}"));
                0
            }
        };
        let input = if n > 0 || done { enc.dequeue_input_buffer(timeout) } else { Ok(DequeuedInputBufferResult::TryAgainLater) };
        if let Ok(DequeuedInputBufferResult::Buffer(mut b)) = input {
            let dst = b.buffer_mut();
            let bytes = n * 2;
            for (i, s) in pcm[..n].iter().enumerate() {
                let le = s.to_le_bytes();
                dst[i * 2].write(le[0]);
                dst[i * 2 + 1].write(le[1]);
            }
            let pts = (frames * 1_000_000 / rate as u64) as u64;
            let _ = enc.queue_input_buffer(b, 0, bytes, pts, if done { EOS } else { 0 });
            frames += n as u64;
        }
        if done {
            for _ in 0..40 {
                if drain(&enc, &shared, false, Duration::from_millis(50)) {
                    break;
                }
            }
            break;
        }
        drain(&enc, &shared, false, Duration::ZERO);
    }
    let _ = stream.request_stop();
    let _ = enc.stop();
    Some(())
}

impl Recorder {
    /// `fd` is a writable file descriptor the recorder takes ownership of.
    pub fn start(fd: i32, width: u32, height: u32, with_audio: bool) -> Result<Recorder, String> {
        let file = unsafe { File::from_raw_fd(fd) };
        let w = Mp4Writer::new(file).map_err(|e| e.to_string())?;
        let shared: Shared = Arc::new(Mutex::new(Mux { w: Some(w), video: None, audio: None }));

        let mut f = MediaFormat::new();
        f.set_str("mime", "video/avc");
        f.set_i32("width", width as i32);
        f.set_i32("height", height as i32);
        f.set_i32("color-format", 21);
        f.set_i32("bitrate", (width * height * 6).min(12_000_000) as i32);
        f.set_i32("frame-rate", 30);
        f.set_i32("i-frame-interval", 1);
        let venc = MediaCodec::from_encoder_type("video/avc").ok_or("no H.264 encoder")?;
        venc.configure(&f, None, MediaCodecDirection::Encoder).map_err(|e| format!("encoder config: {e}"))?;
        venc.start().map_err(|e| format!("encoder start: {e}"))?;

        let stop_audio = Arc::new(AtomicBool::new(false));
        let audio_thread = with_audio.then(|| {
            let (s, st) = (shared.clone(), stop_audio.clone());
            std::thread::spawn(move || {
                if audio_loop(s, st).is_none() {
                    eprintln!("audio capture unavailable");
                }
            })
        });
        Ok(Recorder { venc, shared, stop_audio, audio_thread, first_ts_ns: None })
    }

    pub fn push_video(&mut self, frame: &Nv12, ts_ns: i64) {
        let first = *self.first_ts_ns.get_or_insert(ts_ns);
        let pts = ((ts_ns - first) / 1000).max(0) as u64;
        if let Ok(DequeuedInputBufferResult::Buffer(mut b)) = self.venc.dequeue_input_buffer(Duration::from_millis(10)) {
            let dst = b.buffer_mut();
            if dst.len() >= frame.data.len() {
                for (d, s) in dst.iter_mut().zip(&frame.data) {
                    d.write(*s);
                }
                let _ = self.venc.queue_input_buffer(b, 0, frame.data.len(), pts, 0);
            } else {
                let _ = self.venc.queue_input_buffer(b, 0, 0, pts, 0);
            }
        }
        drain(&self.venc, &self.shared, true, Duration::ZERO);
    }

    /// Flushes the encoders and closes the file.
    pub fn finish(mut self) -> Result<(), String> {
        if let Ok(DequeuedInputBufferResult::Buffer(b)) = self.venc.dequeue_input_buffer(Duration::from_millis(200)) {
            let _ = self.venc.queue_input_buffer(b, 0, 0, 0, EOS);
        }
        for _ in 0..40 {
            if drain(&self.venc, &self.shared, true, Duration::from_millis(50)) {
                break;
            }
        }
        let _ = self.venc.stop();
        self.stop_audio.store(true, Ordering::SeqCst);
        if let Some(t) = self.audio_thread.take() {
            let _ = t.join();
        }
        let w = self.shared.lock().unwrap().w.take().ok_or("already finished")?;
        w.finish().map_err(|e| e.to_string())?;
        Ok(())
    }
}
