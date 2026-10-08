//! Minimal MP4 muxer (H.264 + AAC). Every wall-clock field (creation/modification time) is zero and
//! no vendor/handler names, location or user-data boxes are written, so a recording carries only
//! what is needed to play it.

use std::io::{Result, Seek, SeekFrom, Write};

pub struct VideoConfig {
    pub width: u16,
    pub height: u16,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// AudioSpecificConfig bytes from the AAC encoder.
    pub asc: Vec<u8>,
}

enum Kind {
    Video(VideoConfig),
    Audio(AudioConfig),
}

struct Track {
    kind: Kind,
    timescale: u32,
    /// (file offset, size, pts in timescale units, sync)
    samples: Vec<(u64, u32, u64, bool)>,
}

pub struct Mp4Writer<W: Write + Seek> {
    out: W,
    tracks: Vec<Track>,
    mdat_start: u64,
    pos: u64,
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 8);
    v.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

fn full(kind: &[u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = version_flags.to_be_bytes().to_vec();
    b.extend_from_slice(body);
    boxed(kind, &b)
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

impl<W: Write + Seek> Mp4Writer<W> {
    pub fn new(mut out: W) -> Result<Self> {
        let ftyp = boxed(b"ftyp", &cat(&[b"isom".to_vec(), u32s(&[0]), b"isomiso2avc1mp41".to_vec()]));
        out.write_all(&ftyp)?;
        // mdat with a 64-bit size, patched in `finish`.
        let mdat_start = ftyp.len() as u64;
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&0u64.to_be_bytes())?;
        Ok(Mp4Writer { out, tracks: vec![], mdat_start, pos: mdat_start + 16 })
    }

    pub fn add_video(&mut self, cfg: VideoConfig) -> usize {
        self.tracks.push(Track { kind: Kind::Video(cfg), timescale: 90_000, samples: vec![] });
        self.tracks.len() - 1
    }

    pub fn add_audio(&mut self, cfg: AudioConfig) -> usize {
        let timescale = cfg.sample_rate;
        self.tracks.push(Track { kind: Kind::Audio(cfg), timescale, samples: vec![] });
        self.tracks.len() - 1
    }

    /// Writes one sample. Video samples are Annex-B (start-code) H.264 and are converted to
    /// length-prefixed NAL units; audio samples are raw AAC frames.
    pub fn write_sample(&mut self, track: usize, data: &[u8], pts_us: i64, sync: bool) -> Result<()> {
        let t = &mut self.tracks[track];
        let payload = match t.kind {
            Kind::Video(_) => annexb_to_length_prefixed(data),
            Kind::Audio(_) => data.to_vec(),
        };
        let pts = (pts_us.max(0) as u128 * t.timescale as u128 / 1_000_000) as u64;
        t.samples.push((self.pos, payload.len() as u32, pts, sync));
        self.out.write_all(&payload)?;
        self.pos += payload.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        let mdat_size = self.pos - self.mdat_start;
        self.out.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.out.write_all(&mdat_size.to_be_bytes())?;
        self.out.seek(SeekFrom::Start(self.pos))?;
        let moov = self.moov();
        self.out.write_all(&moov)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn track_duration_ms(t: &Track) -> u64 {
        Self::duration(t) * 1000 / t.timescale as u64
    }

    fn deltas(t: &Track) -> Vec<u32> {
        let mut d: Vec<u32> = t.samples.windows(2).map(|w| w[1].2.saturating_sub(w[0].2) as u32).collect();
        let last = d.last().copied().unwrap_or(t.timescale / 30);
        d.push(last);
        d
    }

    fn duration(t: &Track) -> u64 {
        Self::deltas(t).iter().map(|&d| d as u64).sum()
    }

    fn moov(&self) -> Vec<u8> {
        let dur = self.tracks.iter().map(Self::track_duration_ms).max().unwrap_or(0) as u32;
        let mvhd = full(
            b"mvhd",
            0,
            &cat(&[u32s(&[0, 0, 1000, dur]), u32s(&[0x10000]), vec![1, 0], vec![0; 10], u32s(&MATRIX), vec![0; 24], u32s(&[self.tracks.len() as u32 + 1])]),
        );
        let mut parts = vec![mvhd];
        for (i, t) in self.tracks.iter().enumerate() {
            parts.push(self.trak(i as u32 + 1, t));
        }
        boxed(b"moov", &cat(&parts))
    }

    fn trak(&self, id: u32, t: &Track) -> Vec<u8> {
        let (w, h) = match &t.kind {
            Kind::Video(v) => ((v.width as u32) << 16, (v.height as u32) << 16),
            Kind::Audio(_) => (0, 0),
        };
        let volume: u16 = if matches!(t.kind, Kind::Audio(_)) { 0x0100 } else { 0 };
        let tkhd = full(
            b"tkhd",
            3,
            &cat(&[u32s(&[0, 0, id, 0, Self::track_duration_ms(t) as u32]), vec![0; 8], vec![0; 4], volume.to_be_bytes().to_vec(), vec![0; 2], u32s(&MATRIX), u32s(&[w, h])]),
        );
        let mdhd = full(b"mdhd", 0, &cat(&[u32s(&[0, 0, t.timescale, Self::duration(t) as u32]), vec![0x55, 0xc4, 0, 0]]));
        let (handler, header) = match &t.kind {
            Kind::Video(_) => (b"vide", full(b"vmhd", 1, &[0; 8])),
            Kind::Audio(_) => (b"soun", full(b"smhd", 0, &[0; 4])),
        };
        let hdlr = full(b"hdlr", 0, &cat(&[u32s(&[0]), handler.to_vec(), vec![0; 12], vec![0]]));
        let dinf = boxed(b"dinf", &full(b"dref", 0, &cat(&[u32s(&[1]), full(b"url ", 1, &[])])));
        let minf = boxed(b"minf", &cat(&[header, dinf, self.stbl(t)]));
        let mdia = boxed(b"mdia", &cat(&[mdhd, hdlr, minf]));
        boxed(b"trak", &cat(&[tkhd, mdia]))
    }

    fn stbl(&self, t: &Track) -> Vec<u8> {
        let stsd = match &t.kind {
            Kind::Video(v) => {
                let avcc = boxed(
                    b"avcC",
                    &cat(&[
                        vec![1, v.sps[1], v.sps[2], v.sps[3], 0xff, 0xe1],
                        (v.sps.len() as u16).to_be_bytes().to_vec(),
                        v.sps.clone(),
                        vec![1],
                        (v.pps.len() as u16).to_be_bytes().to_vec(),
                        v.pps.clone(),
                    ]),
                );
                let entry = cat(&[
                    vec![0; 6],
                    1u16.to_be_bytes().to_vec(),
                    vec![0; 16],
                    v.width.to_be_bytes().to_vec(),
                    v.height.to_be_bytes().to_vec(),
                    u32s(&[0x48_0000, 0x48_0000, 0]),
                    1u16.to_be_bytes().to_vec(),
                    vec![0; 32],
                    0x18u16.to_be_bytes().to_vec(),
                    0xffffu16.to_be_bytes().to_vec(),
                    avcc,
                ]);
                full(b"stsd", 0, &cat(&[u32s(&[1]), boxed(b"avc1", &entry)]))
            }
            Kind::Audio(a) => {
                let asc = &a.asc;
                let dsi = cat(&[vec![5, asc.len() as u8], asc.clone()]);
                let dcd = cat(&[vec![4, (13 + dsi.len()) as u8, 0x40, 0x15, 0, 0, 0], u32s(&[0, 0]), dsi]);
                let es = cat(&[vec![3, (3 + dcd.len() + 3) as u8, 0, 0, 0], dcd, vec![6, 1, 2]]);
                let entry = cat(&[
                    vec![0; 6],
                    1u16.to_be_bytes().to_vec(),
                    vec![0; 8],
                    a.channels.to_be_bytes().to_vec(),
                    16u16.to_be_bytes().to_vec(),
                    vec![0; 4],
                    (a.sample_rate << 16).to_be_bytes().to_vec(),
                    full(b"esds", 0, &es),
                ]);
                full(b"stsd", 0, &cat(&[u32s(&[1]), boxed(b"mp4a", &entry)]))
            }
        };
        let deltas = Self::deltas(t);
        let mut runs: Vec<(u32, u32)> = vec![];
        for d in deltas {
            match runs.last_mut() {
                Some((n, rd)) if *rd == d => *n += 1,
                _ => runs.push((1, d)),
            }
        }
        let stts = full(b"stts", 0, &cat(&[u32s(&[runs.len() as u32]), runs.iter().flat_map(|&(n, d)| u32s(&[n, d])).collect()]));
        let stsc = full(b"stsc", 0, &u32s(&[1, 1, 1, 1]));
        let stsz = full(b"stsz", 0, &cat(&[u32s(&[0, t.samples.len() as u32]), t.samples.iter().flat_map(|s| u32s(&[s.1])).collect()]));
        let co64 = full(
            b"co64",
            0,
            &cat(&[u32s(&[t.samples.len() as u32]), t.samples.iter().flat_map(|s| s.0.to_be_bytes()).collect()]),
        );
        let mut parts = vec![stsd, stts];
        if matches!(t.kind, Kind::Video(_)) {
            let sync: Vec<u32> = t.samples.iter().enumerate().filter(|(_, s)| s.3).map(|(i, _)| i as u32 + 1).collect();
            parts.push(full(b"stss", 0, &cat(&[u32s(&[sync.len() as u32]), u32s(&sync)])));
        }
        parts.extend([stsc, stsz, co64]);
        boxed(b"stbl", &cat(&parts))
    }
}

/// Splits an Annex-B stream into NAL units (without start codes).
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let mut e = starts.get(n + 1).map(|&x| x - 3).unwrap_or(data.len());
            while e > s && data[e - 1] == 0 {
                e -= 1;
            }
            &data[s..e]
        })
        .collect()
}

fn annexb_to_length_prefixed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for nal in split_annexb(data) {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn splits_annexb() {
        let d = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3];
        let n = split_annexb(&d);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..]]);
    }

    #[test]
    fn has_no_timestamps_or_vendor_strings() {
        let mut w = Mp4Writer::new(Cursor::new(Vec::new())).unwrap();
        let v = w.add_video(VideoConfig { width: 64, height: 48, sps: vec![0x67, 0x42, 0xc0, 0x1e, 1], pps: vec![0x68, 1] });
        let a = w.add_audio(AudioConfig { sample_rate: 44100, channels: 1, asc: vec![0x12, 0x08] });
        for i in 0..5 {
            w.write_sample(v, &[0, 0, 0, 1, 0x65, i], i as i64 * 33_333, i == 0).unwrap();
            w.write_sample(a, &[1, 2, 3], i as i64 * 23_220, true).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        let pos = bytes.windows(4).position(|w| w == b"mvhd").unwrap();
        // version/flags (4) then creation + modification time
        assert_eq!(&bytes[pos + 8..pos + 16], &[0; 8]);
        for s in [&b"udta"[..], b"meta", b"loc", b"xyz", b"Lavf", b"Google"] {
            assert!(!bytes.windows(s.len()).any(|w| w == s));
        }
    }
}
