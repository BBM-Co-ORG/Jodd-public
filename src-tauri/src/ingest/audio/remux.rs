//! Copy a clip's audio track into an audio-only `.m4a` — no decode, no
//! re-encode (spec D4, §5.3).
//!
//! TikTok sends HE-AACv2 and Instagram HE-AAC, which no pure-Rust decoder
//! handles, so nothing here looks inside a sample. The decoder config
//! (`stsd` → `mp4a` → `esds`) and the edit list are copied as raw bytes, the
//! samples are copied chunk by chunk, and only the chunk offsets are new.
//! A purpose reader/writer rather than an MP4 crate: the guarantee that
//! matters is "these bytes, untouched", and one audio track needs only a
//! small subset of ISO-BMFF.
//!
//! Reads `moov` before or after `mdat`, and 32- or 64-bit chunk offsets.
//! Fragmented MP4 (`moof`) is refused; no measured platform sends it.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct RemuxedAudio {
    pub bytes: u64,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RemuxError {
    #[error("the clip has no sound")]
    NoAudio,
    #[error("not a video file")]
    NotMp4,
    #[error("{0}")]
    Other(String),
}

impl From<io::Error> for RemuxError {
    fn from(e: io::Error) -> Self {
        RemuxError::Other(format!("audio remux: {e}"))
    }
}

/// The first box of a real MP4. Anything else — an HTML login wall, an
/// empty body — is not a video file.
const FIRST_BOXES: [&[u8; 4]; 4] = [b"ftyp", b"moov", b"mdat", b"free"];
/// `moov` is read whole: a 30-minute clip's is a few MB. Bounds a hostile one.
const MAX_MOOV_BYTES: u64 = 16 * 1024 * 1024;
/// The most entries any sample table may have — samples, chunks, durations.
/// 1800 s (D7's cap) of 48 kHz AAC is ~85 000 samples; this is ~10× that.
/// A constant-size `stsz` states its count without paying for it in bytes,
/// and each entry costs memory and output bytes, so it is bounded here.
const MAX_SAMPLES: u32 = 1_000_000;
const FRAGMENTED: &str = "fragmented MP4 is not supported";

fn damaged() -> RemuxError {
    RemuxError::Other("the video file is damaged".into())
}

fn incomplete() -> RemuxError {
    RemuxError::Other("the video file is incomplete".into())
}

/// Write `src`'s first sound track to `dest` as a fast-start `.m4a`. On any
/// error `dest` does not exist afterwards.
pub fn remux_audio_only(src: &Path, dest: &Path) -> Result<RemuxedAudio, RemuxError> {
    let mut input = File::open(src)?;
    let (track, chunks) = read_audio_track(&mut input)?;
    let written = write_m4a(&mut input, &track, &chunks, dest, false);
    if written.is_err() {
        let _ = fs::remove_file(dest);
    }
    Ok(RemuxedAudio { bytes: written?, duration_secs: track.duration_secs() })
}

// ---- reading -------------------------------------------------------------

/// What the output needs from the source's first `soun` track.
struct AudioTrack {
    movie_timescale: u32,
    /// `mdhd`'s: the unit of `stts`.
    timescale: u32,
    /// The `stts` total, in `timescale`.
    media_duration: u64,
    language: u16,
    /// The raw `edts` box, kept when its `elst` parses: it is what tells a
    /// player to skip the encoder's priming samples, and what ffprobe's
    /// duration means.
    edts: Option<Vec<u8>>,
    /// The edit list's total, in `movie_timescale`.
    edited_duration: Option<u64>,
    /// The raw `stsd` box — `mp4a` and its `esds`, byte for byte.
    stsd: Vec<u8>,
    /// `(sample_count, sample_delta)`.
    stts: Vec<(u32, u32)>,
    stsc: Vec<StscEntry>,
    sample_sizes: Vec<u32>,
    chunk_offsets: Vec<u64>,
}

#[derive(Debug, Clone, Copy)]
struct StscEntry {
    first_chunk: u32,
    samples_per_chunk: u32,
    description_index: u32,
}

/// One chunk's samples: contiguous in the file by definition of a chunk.
struct Chunk {
    offset: u64,
    len: u64,
}

impl AudioTrack {
    fn movie_duration(&self) -> u64 {
        self.edited_duration.unwrap_or_else(|| {
            let scaled = u128::from(self.media_duration) * u128::from(self.movie_timescale)
                / u128::from(self.timescale);
            u64::try_from(scaled).unwrap_or(u64::MAX)
        })
    }

    fn duration_secs(&self) -> f64 {
        match self.edited_duration {
            Some(d) => d as f64 / f64::from(self.movie_timescale),
            None => self.media_duration as f64 / f64::from(self.timescale),
        }
    }
}

fn read_audio_track(file: &mut File) -> Result<(AudioTrack, Vec<Chunk>), RemuxError> {
    let len = file.metadata()?.len();
    let (moov_at, moov_len) = find_moov(file, len)?;
    if moov_len > MAX_MOOV_BYTES {
        return Err(RemuxError::Other("the video index is too large".into()));
    }
    let mut moov = vec![0; moov_len as usize];
    file.seek(SeekFrom::Start(moov_at))?;
    file.read_exact(&mut moov)?;
    let track = parse_moov(&moov)?;
    let chunks = chunk_layout(&track, len)?;
    Ok((track, chunks))
}

/// Walk the top-level boxes; return the `moov` body's offset and length.
fn find_moov(file: &mut File, len: u64) -> Result<(u64, u64), RemuxError> {
    if len < 8 {
        return Err(RemuxError::NotMp4);
    }
    let mut at = 0;
    let mut moov = None;
    while at < len {
        let mut head = [0u8; 16];
        let n = (len - at).min(16) as usize;
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut head[..n])?;
        let Some((header, size)) = box_header(&head[..n], len - at) else {
            if at == 0 {
                return Err(RemuxError::NotMp4);
            }
            // A cut-short last box. If `moov` came first the samples can
            // still be checked against the file's length.
            break;
        };
        let kind = &head[4..8];
        if at == 0 && !FIRST_BOXES.iter().any(|k| &k[..] == kind) {
            return Err(RemuxError::NotMp4);
        }
        match kind {
            b"moof" => return Err(RemuxError::Other(FRAGMENTED.into())),
            b"moov" if moov.is_none() => moov = Some((at + header as u64, size - header as u64)),
            _ => {}
        }
        at += size;
    }
    moov.ok_or_else(incomplete)
}

/// `(header_len, box_len)` for the box at the start of `head`, if it is
/// well formed and fits in `remaining`. Size 0 means "to the end".
fn box_header(head: &[u8], remaining: u64) -> Option<(usize, u64)> {
    let size32 = u32::from_be_bytes(head.get(0..4)?.try_into().ok()?);
    head.get(4..8)?;
    let (header, size) = match size32 {
        0 => (8, remaining),
        1 => (16, u64::from_be_bytes(head.get(8..16)?.try_into().ok()?)),
        n => (8, u64::from(n)),
    };
    (size >= header as u64 && size <= remaining).then_some((header, size))
}

#[derive(Clone, Copy)]
struct Mp4Box<'a> {
    kind: [u8; 4],
    body: &'a [u8],
    raw: &'a [u8],
}

/// The boxes inside a container's body. A tail shorter than a box header
/// is padding some writers leave, not an error.
struct Boxes<'a>(&'a [u8]);

impl<'a> Iterator for Boxes<'a> {
    type Item = Result<Mp4Box<'a>, RemuxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.len() < 8 {
            return None;
        }
        let Some((header, size)) = box_header(self.0, self.0.len() as u64) else {
            self.0 = &[];
            return Some(Err(damaged()));
        };
        let (raw, rest) = self.0.split_at(size as usize);
        self.0 = rest;
        let kind = raw[4..8].try_into().expect("box_header checked 8 bytes");
        Some(Ok(Mp4Box { kind, body: &raw[header..], raw }))
    }
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<Mp4Box<'a>>, RemuxError> {
    for b in Boxes(data) {
        let b = b?;
        if &b.kind == kind {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

fn need<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Mp4Box<'a>, RemuxError> {
    find(data, kind)?.ok_or_else(damaged)
}

fn parse_moov(moov: &[u8]) -> Result<AudioTrack, RemuxError> {
    let mut movie_timescale = None;
    let mut audio = None;
    for b in Boxes(moov) {
        let b = b?;
        match &b.kind {
            // A fragmented file's samples live in `moof`s; its `stbl`s are empty.
            b"mvex" => return Err(RemuxError::Other(FRAGMENTED.into())),
            b"mvhd" => movie_timescale = Some(mvhd_timescale(b.body)?),
            b"trak" if audio.is_none() => audio = parse_trak(b.body)?,
            _ => {}
        }
    }
    let mut track = audio.ok_or(RemuxError::NoAudio)?;
    track.movie_timescale = movie_timescale.filter(|&t| t != 0).ok_or_else(damaged)?;
    if track.sample_sizes.is_empty() {
        return Err(RemuxError::NoAudio);
    }
    Ok(track)
}

/// `Ok(None)` for a track that is not sound.
fn parse_trak(trak: &[u8]) -> Result<Option<AudioTrack>, RemuxError> {
    let Some(mdia) = find(trak, b"mdia")? else { return Ok(None) };
    let Some(hdlr) = find(mdia.body, b"hdlr")? else { return Ok(None) };
    if hdlr_type(hdlr.body)? != *b"soun" {
        return Ok(None);
    }
    let (timescale, language) = mdhd(need(mdia.body, b"mdhd")?.body)?;
    if timescale == 0 {
        return Err(damaged());
    }
    let (edts, edited_duration) = match find(trak, b"edts")? {
        Some(edts) => match find(edts.body, b"elst")?.map(|e| elst_total(e.body)).transpose()? {
            Some(total) if total > 0 => (Some(edts.raw.to_vec()), Some(total)),
            _ => (None, None),
        },
        None => (None, None),
    };
    let minf = need(mdia.body, b"minf")?;
    let stbl = need(minf.body, b"stbl")?.body;
    let chunk_offsets = match (find(stbl, b"stco")?, find(stbl, b"co64")?) {
        (Some(b), _) => entries(&mut Cursor::full(b.body)?, 4, |c| c.u32().map(u64::from))?,
        (None, Some(b)) => entries(&mut Cursor::full(b.body)?, 8, Cursor::u64)?,
        (None, None) => return Err(damaged()),
    };
    let stts = entries(&mut Cursor::full(need(stbl, b"stts")?.body)?, 8, |c| Ok((c.u32()?, c.u32()?)))?;
    let media_duration = stts
        .iter()
        .try_fold(0u64, |sum, &(n, d)| sum.checked_add(u64::from(n) * u64::from(d)))
        .ok_or_else(damaged)?;
    Ok(Some(AudioTrack {
        movie_timescale: 0, // set by parse_moov from mvhd
        timescale,
        media_duration,
        language,
        edts,
        edited_duration,
        stsd: need(stbl, b"stsd")?.raw.to_vec(),
        stts,
        stsc: entries(&mut Cursor::full(need(stbl, b"stsc")?.body)?, 12, |c| {
            Ok(StscEntry {
                first_chunk: c.u32()?,
                samples_per_chunk: c.u32()?,
                description_index: c.u32()?,
            })
        })?,
        sample_sizes: stsz(need(stbl, b"stsz")?.body)?,
        chunk_offsets,
    }))
}

/// Big-endian reads over a box body; running out is `damaged`.
struct Cursor<'a> {
    data: &'a [u8],
    version: u8,
}

impl<'a> Cursor<'a> {
    /// Past a full box's version and flags.
    fn full(body: &'a [u8]) -> Result<Self, RemuxError> {
        let mut c = Cursor { data: body, version: 0 };
        c.version = c.take(4)?[0];
        Ok(c)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RemuxError> {
        if self.data.len() < n {
            return Err(damaged());
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, RemuxError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("2 bytes")))
    }

    fn u32(&mut self) -> Result<u32, RemuxError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn u64(&mut self) -> Result<u64, RemuxError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("8 bytes")))
    }
}

/// A `u32` count, then that many fixed-size entries — the count checked
/// against `MAX_SAMPLES` and the bytes present before anything is allocated.
fn entries<'a, T>(
    c: &mut Cursor<'a>,
    entry_len: usize,
    mut read: impl FnMut(&mut Cursor<'a>) -> Result<T, RemuxError>,
) -> Result<Vec<T>, RemuxError> {
    let count = c.u32()?;
    if count > MAX_SAMPLES || c.data.len() / entry_len < count as usize {
        return Err(damaged());
    }
    (0..count).map(|_| read(c)).collect()
}

fn mvhd_timescale(body: &[u8]) -> Result<u32, RemuxError> {
    let mut c = Cursor::full(body)?;
    c.take(if c.version == 1 { 16 } else { 8 })?; // creation, modification
    c.u32()
}

fn hdlr_type(body: &[u8]) -> Result<[u8; 4], RemuxError> {
    let mut c = Cursor::full(body)?;
    c.take(4)?; // pre_defined
    Ok(c.take(4)?.try_into().expect("4 bytes"))
}

/// `(timescale, language)`.
fn mdhd(body: &[u8]) -> Result<(u32, u16), RemuxError> {
    let mut c = Cursor::full(body)?;
    let v1 = c.version == 1;
    c.take(if v1 { 16 } else { 8 })?; // creation, modification
    let timescale = c.u32()?;
    c.take(if v1 { 8 } else { 4 })?; // duration: rebuilt from stts
    Ok((timescale, c.u16()?))
}

/// The sum of an edit list's segment durations.
fn elst_total(body: &[u8]) -> Result<u64, RemuxError> {
    let mut c = Cursor::full(body)?;
    let v1 = c.version == 1;
    let segments = entries(&mut c, if v1 { 20 } else { 12 }, |c| {
        let duration = if v1 { c.u64()? } else { u64::from(c.u32()?) };
        c.take(if v1 { 8 } else { 4 })?; // media_time
        c.take(4)?; // media_rate
        Ok(duration)
    })?;
    segments.into_iter().try_fold(0u64, |sum, d| sum.checked_add(d)).ok_or_else(damaged)
}

fn stsz(body: &[u8]) -> Result<Vec<u32>, RemuxError> {
    let mut c = Cursor::full(body)?;
    let constant = c.u32()?;
    if constant == 0 {
        return entries(&mut c, 4, Cursor::u32);
    }
    let count = c.u32()?;
    if count > MAX_SAMPLES {
        return Err(damaged());
    }
    Ok(vec![constant; count as usize])
}

/// Where each chunk's samples are, checked against the sample table and
/// the file: a table that does not describe exactly its samples, points
/// past the end of a cut-short download, or points two chunks at the same
/// bytes, is refused. The last is what bounds the output by the input.
fn chunk_layout(track: &AudioTrack, file_len: u64) -> Result<Vec<Chunk>, RemuxError> {
    let inconsistent = || RemuxError::Other("the video's sample table is inconsistent".into());
    let stsc = &track.stsc;
    if stsc.first().map(|e| e.first_chunk) != Some(1)
        || stsc.windows(2).any(|w| w[1].first_chunk <= w[0].first_chunk)
        || stsc.iter().any(|e| e.samples_per_chunk == 0)
        || track.chunk_offsets.len() > track.sample_sizes.len()
    {
        return Err(inconsistent());
    }
    let stts_samples: u64 = track.stts.iter().map(|&(n, _)| u64::from(n)).sum();
    if stts_samples != track.sample_sizes.len() as u64 {
        return Err(inconsistent());
    }
    let mut chunks = Vec::with_capacity(track.chunk_offsets.len());
    let mut sample = 0usize;
    let mut entry = 0;
    for (i, &offset) in track.chunk_offsets.iter().enumerate() {
        let number = i as u64 + 1;
        while stsc.get(entry + 1).is_some_and(|e| u64::from(e.first_chunk) <= number) {
            entry += 1;
        }
        let end = sample
            .checked_add(stsc[entry].samples_per_chunk as usize)
            .filter(|&end| end <= track.sample_sizes.len())
            .ok_or_else(inconsistent)?;
        let len: u64 = track.sample_sizes[sample..end].iter().map(|&s| u64::from(s)).sum();
        if offset.checked_add(len).is_none_or(|e| e > file_len) {
            return Err(incomplete());
        }
        chunks.push(Chunk { offset, len });
        sample = end;
    }
    if sample != track.sample_sizes.len() {
        return Err(inconsistent());
    }
    // A real file's chunks are disjoint parts of it.
    let mut extents: Vec<(u64, u64)> =
        chunks.iter().filter(|c| c.len > 0).map(|c| (c.offset, c.offset + c.len)).collect();
    extents.sort_unstable();
    if extents.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(inconsistent());
    }
    Ok(chunks)
}

// ---- writing -------------------------------------------------------------

/// Write `ftyp`, `moov`, then `mdat` with every chunk in source order.
/// Returns the file's length.
fn write_m4a(
    input: &mut File,
    track: &AudioTrack,
    chunks: &[Chunk],
    dest: &Path,
    force_co64: bool,
) -> Result<u64, RemuxError> {
    let data_len: u64 = chunks.iter().map(|c| c.len).sum();
    let ftyp = ftyp();
    let mdat_header: u64 = if data_len + 8 > u64::from(u32::MAX) { 16 } else { 8 };
    // 32-bit offsets unless the file passes 4 GiB, which costs one rebuild.
    let fits = |moov: &[u8]| ftyp.len() as u64 + moov.len() as u64 + mdat_header + data_len <= u64::from(u32::MAX);
    let mut co64 = force_co64;
    let (mut index, mut table_at) = moov(track, chunks, co64);
    if !co64 && !fits(&index) {
        co64 = true;
        (index, table_at) = moov(track, chunks, co64);
    }
    let data_start = ftyp.len() as u64 + index.len() as u64 + mdat_header;
    let mut at = data_start;
    for (i, chunk) in chunks.iter().enumerate() {
        if co64 {
            index[table_at + 8 * i..][..8].copy_from_slice(&at.to_be_bytes());
        } else {
            index[table_at + 4 * i..][..4].copy_from_slice(&(at as u32).to_be_bytes());
        }
        at += chunk.len;
    }

    let mut out = BufWriter::new(File::create(dest)?);
    out.write_all(&ftyp)?;
    out.write_all(&index)?;
    if mdat_header == 16 {
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&(data_len + 16).to_be_bytes())?;
    } else {
        out.write_all(&((data_len + 8) as u32).to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    for chunk in chunks {
        input.seek(SeekFrom::Start(chunk.offset))?;
        if io::copy(&mut (&mut *input).take(chunk.len), &mut out)? != chunk.len {
            return Err(incomplete());
        }
    }
    out.into_inner().map_err(|e| e.into_error())?;
    Ok(data_start + data_len)
}

/// One growing buffer. A box is opened with a placeholder size and closed by
/// patching it, so a nested box is written once rather than copied into
/// each parent.
struct Boxed(Vec<u8>);

impl Boxed {
    fn open(&mut self, kind: &[u8; 4]) -> usize {
        let at = self.0.len();
        self.0.extend_from_slice(&[0; 4]);
        self.0.extend_from_slice(kind);
        at
    }

    fn open_full(&mut self, kind: &[u8; 4], version: u8, flags: u32) -> usize {
        let at = self.open(kind);
        self.u32s(&[(u32::from(version) << 24) | flags]);
        at
    }

    fn close(&mut self, at: usize) {
        let len = (self.0.len() - at) as u32;
        self.0[at..at + 4].copy_from_slice(&len.to_be_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }

    fn u32s(&mut self, words: &[u32]) {
        for w in words {
            self.0.extend_from_slice(&w.to_be_bytes());
        }
    }

    /// Zero creation and modification times, then `middle` (timescale or
    /// track id), then the duration — all 64-bit-timed when `v1`.
    fn times(&mut self, v1: bool, middle: &[u32], duration: u64) {
        self.bytes(&[0; 16][..if v1 { 16 } else { 8 }]);
        self.u32s(middle);
        if v1 {
            self.bytes(&duration.to_be_bytes());
        } else {
            self.u32s(&[duration as u32]);
        }
    }
}

fn ftyp() -> Vec<u8> {
    let mut b = Boxed(Vec::new());
    let ftyp = b.open(b"ftyp");
    b.bytes(b"M4A ");
    b.u32s(&[0]);
    b.bytes(b"M4A isommp42");
    b.close(ftyp);
    b.0
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// The output's `moov`, and where its chunk-offset table starts. The table
/// is left zeroed for `write_m4a` to fill once the moov's length is known.
fn moov(track: &AudioTrack, chunks: &[Chunk], co64: bool) -> (Vec<u8>, usize) {
    let movie = track.movie_duration();
    let movie_v1 = movie > u64::from(u32::MAX);
    let media_v1 = track.media_duration > u64::from(u32::MAX);
    let mut b = Boxed(Vec::new());
    let moov = b.open(b"moov");

    let mvhd = b.open_full(b"mvhd", movie_v1.into(), 0);
    b.times(movie_v1, &[track.movie_timescale], movie);
    b.u32s(&[0x0001_0000]); // rate 1.0
    b.bytes(&[0x01, 0x00]); // volume 1.0
    b.bytes(&[0; 10]);
    b.u32s(&MATRIX);
    b.bytes(&[0; 24]);
    b.u32s(&[2]); // next_track_ID
    b.close(mvhd);

    let trak = b.open(b"trak");
    let tkhd = b.open_full(b"tkhd", movie_v1.into(), 0x3); // enabled, in movie
    b.times(movie_v1, &[1, 0], movie); // track_ID 1, reserved
    b.bytes(&[0; 8]);
    b.bytes(&[0, 0, 0, 0, 0x01, 0x00, 0, 0]); // layer, group, volume 1.0
    b.u32s(&MATRIX);
    b.u32s(&[0, 0]); // width, height
    b.close(tkhd);
    if let Some(edts) = &track.edts {
        b.bytes(edts);
    }

    let mdia = b.open(b"mdia");
    let mdhd = b.open_full(b"mdhd", media_v1.into(), 0);
    b.times(media_v1, &[track.timescale], track.media_duration);
    b.bytes(&track.language.to_be_bytes());
    b.bytes(&[0, 0]);
    b.close(mdhd);
    let hdlr = b.open_full(b"hdlr", 0, 0);
    b.bytes(&[0; 4]);
    b.bytes(b"soun");
    b.bytes(&[0; 12]);
    b.bytes(b"SoundHandler\0");
    b.close(hdlr);

    let minf = b.open(b"minf");
    let smhd = b.open_full(b"smhd", 0, 0);
    b.bytes(&[0; 4]);
    b.close(smhd);
    let dinf = b.open(b"dinf");
    let dref = b.open_full(b"dref", 0, 0);
    b.u32s(&[1]);
    let url = b.open_full(b"url ", 0, 1); // self-contained
    b.close(url);
    b.close(dref);
    b.close(dinf);

    let stbl = b.open(b"stbl");
    b.bytes(&track.stsd);
    let stts = b.open_full(b"stts", 0, 0);
    b.u32s(&[track.stts.len() as u32]);
    for &(count, delta) in &track.stts {
        b.u32s(&[count, delta]);
    }
    b.close(stts);
    let stsc_entries: Vec<_> =
        track.stsc.iter().filter(|e| e.first_chunk as usize <= chunks.len()).collect();
    let stsc = b.open_full(b"stsc", 0, 0);
    b.u32s(&[stsc_entries.len() as u32]);
    for e in stsc_entries {
        b.u32s(&[e.first_chunk, e.samples_per_chunk, e.description_index]);
    }
    b.close(stsc);
    let stsz = b.open_full(b"stsz", 0, 0);
    b.u32s(&[0, track.sample_sizes.len() as u32]);
    b.u32s(&track.sample_sizes);
    b.close(stsz);
    let offsets = b.open_full(if co64 { b"co64" } else { b"stco" }, 0, 0);
    b.u32s(&[chunks.len() as u32]);
    let table_at = b.0.len();
    b.0.resize(table_at + chunks.len() * if co64 { 8 } else { 4 }, 0);
    b.close(offsets);

    for at in [stbl, minf, mdia, trak, moov] {
        b.close(at);
    }
    (b.0, table_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::PathBuf;

    /// `ffprobe -show_streams` on the fixture's audio stream (README).
    const FFPROBE_AUDIO_SECS: f64 = 3.016009;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ingest/fixtures").join(name)
    }

    /// The audio track as this module's reader sees it: every sample's size,
    /// every sample's bytes in order, and the raw `stsd` box.
    fn audio_samples(path: &Path) -> (Vec<u32>, Vec<u8>, Vec<u8>) {
        let mut file = File::open(path).unwrap();
        let (track, chunks) = read_audio_track(&mut file).unwrap();
        let mut data = Vec::new();
        for chunk in &chunks {
            file.seek(SeekFrom::Start(chunk.offset)).unwrap();
            let mut buf = vec![0; chunk.len as usize];
            file.read_exact(&mut buf).unwrap();
            data.extend_from_slice(&buf);
        }
        assert_eq!(data.len() as u64, track.sample_sizes.iter().map(|&s| s as u64).sum::<u64>());
        (track.sample_sizes, data, track.stsd)
    }

    /// The `esds` box (AudioSpecificConfig) inside a raw `stsd`.
    fn esds(stsd: &[u8]) -> &[u8] {
        let at = stsd.windows(4).position(|w| w == b"esds").expect("no esds") - 4;
        let size = u32::from_be_bytes(stsd[at..at + 4].try_into().unwrap()) as usize;
        &stsd[at..at + size]
    }

    /// Top-level box types in file order.
    fn top_level_kinds(bytes: &[u8]) -> Vec<String> {
        let mut kinds = Vec::new();
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            kinds.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
            at += size;
        }
        kinds
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack.windows(needle.len()).filter(|w| *w == needle).count()
    }

    fn assert_is_audio_only_copy_of(src: &Path, out: &Path, result: &RemuxedAudio) {
        let bytes = fs::read(out).unwrap();
        assert_eq!(result.bytes, bytes.len() as u64);
        assert_eq!(&bytes[4..12], b"ftypM4A ");
        assert_eq!(top_level_kinds(&bytes), ["ftyp", "moov", "mdat"], "fast-start layout");
        assert_eq!(count(&bytes, b"trak"), 1, "one track");
        assert_eq!(count(&bytes, b"soun"), 1);
        assert_eq!(count(&bytes, b"vide"), 0);

        let (src_sizes, src_data, src_stsd) = audio_samples(src);
        let (out_sizes, out_data, out_stsd) = audio_samples(out);
        assert_eq!(out_sizes, src_sizes);
        assert!(out_data == src_data, "sample bytes differ");
        assert_eq!(esds(&out_stsd), esds(&src_stsd));
        assert_eq!(out_stsd, src_stsd);
        assert!(
            (result.duration_secs - FFPROBE_AUDIO_SECS).abs() < 0.05,
            "duration {} vs ffprobe {FFPROBE_AUDIO_SECS}",
            result.duration_secs
        );
    }

    #[test]
    fn remux_keeps_the_he_aac_samples_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let src = fixture("audio_he_aac.mp4");
        let out = dir.path().join("a.m4a");
        let result = remux_audio_only(&src, &out).unwrap();
        assert_is_audio_only_copy_of(&src, &out, &result);
        // ~1 MB instead of ~28 MB is the point; the fixture's video is tiny,
        // so only require that the video's bytes are gone.
        assert!(result.bytes < fs::metadata(&src).unwrap().len());
    }

    #[test]
    fn remux_handles_moov_at_end() {
        let src = fixture("audio_moov_at_end.mp4");
        let head = fs::read(&src).unwrap();
        assert_eq!(top_level_kinds(&head), ["ftyp", "free", "mdat", "moov"], "fixture shape");

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.m4a");
        let result = remux_audio_only(&src, &out).unwrap();
        assert_is_audio_only_copy_of(&src, &out, &result);
        // Same audio as the fast-start fixture.
        assert_eq!(audio_samples(&out).1, audio_samples(&fixture("audio_he_aac.mp4")).1);
    }

    #[test]
    fn remux_reports_no_audio() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.m4a");
        let err = remux_audio_only(&fixture("video_no_audio.mp4"), &out).unwrap_err();
        assert_eq!(err, RemuxError::NoAudio);
        assert_eq!(err.to_string(), "the clip has no sound");
        assert!(!out.exists(), "no file is left for a clip without sound");
    }

    #[test]
    fn remux_refuses_html() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.m4a");
        for (name, body) in [
            ("login.html", &b"<!DOCTYPE html><html><body>Log in to continue</body></html>"[..]),
            ("empty.mp4", &b""[..]),
            ("short.mp4", &b"\0\0\0\x20ftyp"[..]),
        ] {
            let src = dir.path().join(name);
            fs::write(&src, body).unwrap();
            let err = remux_audio_only(&src, &out).unwrap_err();
            assert_eq!(err, RemuxError::NotMp4, "{name}");
            assert_eq!(err.to_string(), "not a video file");
            assert!(!out.exists());
        }
    }

    #[test]
    fn remux_reads_and_writes_64_bit_chunk_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let src = fixture("audio_moov_at_end.mp4");
        let wide = dir.path().join("wide.m4a");
        let mut input = File::open(&src).unwrap();
        let (track, chunks) = read_audio_track(&mut input).unwrap();
        write_m4a(&mut input, &track, &chunks, &wide, true).unwrap();
        let bytes = fs::read(&wide).unwrap();
        assert_eq!((count(&bytes, b"co64"), count(&bytes, b"stco")), (1, 0));

        let out = dir.path().join("a.m4a");
        let result = remux_audio_only(&wide, &out).unwrap();
        assert_is_audio_only_copy_of(&src, &out, &result);
        let bytes = fs::read(&out).unwrap();
        assert_eq!((count(&bytes, b"co64"), count(&bytes, b"stco")), (0, 1));
    }

    #[test]
    fn remux_refuses_fragmented_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.m4a");
        let ftyp = tbox(b"ftyp", &[b"iso6", &[0; 4], b"iso6"]);
        let mvhd = tfull(b"mvhd", &[0; 96]);
        // An init segment announcing fragments, and fragments without one.
        for (name, file) in [
            ("init.mp4", [&ftyp[..], &tbox(b"moov", &[&mvhd, &tbox(b"mvex", &[])])].concat()),
            ("frag.mp4", [&ftyp[..], &tbox(b"moof", &[]), &tbox(b"mdat", &[])].concat()),
        ] {
            let src = dir.path().join(name);
            fs::write(&src, file).unwrap();
            let err = remux_audio_only(&src, &out).unwrap_err();
            assert_eq!(err, RemuxError::Other("fragmented MP4 is not supported".into()), "{name}");
        }
    }

    #[test]
    fn remux_refuses_a_cut_short_download() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = fs::read(fixture("audio_he_aac.mp4")).unwrap();
        bytes.truncate(bytes.len() - 100);
        let src = dir.path().join("cut.mp4");
        fs::write(&src, bytes).unwrap();
        let out = dir.path().join("a.m4a");
        let err = remux_audio_only(&src, &out).unwrap_err();
        assert_eq!(err.to_string(), "the video file is incomplete");
        assert!(!out.exists());
    }

    fn tbox(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
        let len = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
        [&(len as u32).to_be_bytes()[..], kind, &parts.concat()].concat()
    }

    fn tfull(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        tbox(kind, &[&[0; 4], body])
    }

    fn words(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// A one-track sound file built from a hand-written sample table:
    /// `stsc` triples, `stsz` (constant size, count, sizes…) and chunk
    /// offsets relative to the start of `mdat`'s `payload` bytes.
    fn crafted(stts: &[u32], stsc: &[u32], stsz: &[u32], offsets: &[u32], payload: usize) -> Vec<u8> {
        let build = |base: u32| {
            let mut mvhd = words(&[0, 0, 1000, 0, 0x0001_0000]);
            mvhd.resize(96, 0);
            let mdhd = words(&[0, 0, 44100, 0, 0x55c4_0000]);
            let hdlr = [&[0u8; 4][..], b"soun", &[0; 12], b"x\0"].concat();
            let stco: Vec<u32> = offsets.iter().map(|o| o + base).collect();
            let stbl = tbox(
                b"stbl",
                &[
                    &tfull(b"stsd", &words(&[0])),
                    &tfull(b"stts", &words(&[&[stts.len() as u32 / 2], stts].concat())),
                    &tfull(b"stsc", &words(&[&[stsc.len() as u32 / 3], stsc].concat())),
                    &tfull(b"stsz", &words(stsz)),
                    &tfull(b"stco", &words(&[&[stco.len() as u32], &stco[..]].concat())),
                ],
            );
            let mdia = tbox(b"mdia", &[&tfull(b"mdhd", &mdhd), &tfull(b"hdlr", &hdlr), &tbox(b"minf", &[&stbl])]);
            let moov = tbox(b"moov", &[&tfull(b"mvhd", &mvhd), &tbox(b"trak", &[&mdia])]);
            let ftyp = tbox(b"ftyp", &[b"isom", &[0; 4], b"isom"]);
            let mdat = tbox(b"mdat", &[&vec![7; payload]]);
            [ftyp, moov, mdat].concat()
        };
        let base = (build(0).len() - payload) as u32;
        build(base)
    }

    #[test]
    fn crafted_tables_are_a_valid_control() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("ok.mp4");
        fs::write(&src, crafted(&[3, 1024], &[1, 1, 1], &[100, 3], &[0, 100, 200], 300)).unwrap();
        let out = dir.path().join("a.m4a");
        let result = remux_audio_only(&src, &out).unwrap();
        assert!(result.bytes < 1000);
        assert_eq!(audio_samples(&out).1, vec![7; 300]);
    }

    /// Box sizes are bounded by the file; what a table *describes* must be
    /// too, or a 1 MB file can claim a 314 MB output.
    #[test]
    fn remux_refuses_a_table_that_describes_more_than_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("a.m4a");
        let many = MAX_SAMPLES + 1;
        for (name, file, why) in [
            // Three chunks over the same 100 bytes.
            ("overlap", crafted(&[3, 1024], &[1, 1, 1], &[100, 3], &[0, 0, 0], 300), "the video's sample table is inconsistent"),
            ("overlap_partly", crafted(&[2, 1024], &[1, 1, 1], &[100, 2], &[0, 50], 300), "the video's sample table is inconsistent"),
            // Chunks that hold nothing, then one that holds the only sample.
            ("zero_per_chunk", crafted(&[1, 1024], &[1, 0, 1, 3, 1, 1], &[100, 1], &[0, 0, 0], 300), "the video's sample table is inconsistent"),
            ("more_chunks_than_samples", crafted(&[1, 1024], &[1, 1, 1], &[100, 1], &[0, 100], 300), "the video's sample table is inconsistent"),
            ("constant_size_too_many", crafted(&[many, 1], &[1, many, 1], &[1, many], &[0], 300), "the video file is damaged"),
            // Durations whose product sum passes u64 (and miscount the samples).
            ("stts_overflow", crafted(&[u32::MAX, u32::MAX, u32::MAX, u32::MAX], &[1, 1, 1], &[100, 1], &[0], 300), "the video file is damaged"),
        ] {
            let src = dir.path().join(format!("{name}.mp4"));
            fs::write(&src, file).unwrap();
            let err = remux_audio_only(&src, &out).unwrap_err();
            assert_eq!(err, RemuxError::Other(why.into()), "{name}");
            assert!(!out.exists(), "{name}");
        }
    }

}
