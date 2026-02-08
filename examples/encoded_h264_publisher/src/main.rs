use clap::{Parser, Subcommand};
use libwebrtc::video_source::native::EncodedH264VideoSource;
use libwebrtc::video_source::{RtcVideoSource, VideoResolution};
use livekit::options::{TrackPublishOptions, VideoCodec};
use livekit::prelude::*;
use log::{info, warn};
use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[derive(Parser, Debug)]
#[command(name = "encoded_h264_publisher")]
#[command(about = "Publish Annex-B H.264 to a LiveKit room")]
struct Cli {
    #[arg(long, global = true)]
    url: Option<String>,
    #[arg(long, global = true)]
    token: Option<String>,
    #[arg(long, default_value_t = 1280, global = true)]
    width: u32,
    #[arg(long, default_value_t = 720, global = true)]
    height: u32,
    #[arg(
        long,
        default_value_t = 30,
        value_parser = clap::value_parser!(u32).range(1..=240),
        global = true
    )]
    fps: u32,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Read a .h264 file from disk.
    File {
        #[arg(long)]
        path: PathBuf,
    },
    /// Read Annex-B H.264 from a TCP stream.
    Tcp {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 5004)]
        port: u16,
    },
}

#[derive(Debug)]
struct AccessUnit {
    data: Vec<u8>,
    is_keyframe: bool,
}

#[derive(Default)]
struct AccessUnitBuilder {
    pending: Vec<u8>,
}

impl AccessUnitBuilder {
    fn push_nal(&mut self, nal: &[u8]) -> Option<AccessUnit> {
        let nal_type = nal_type(nal)?;
        match nal_type {
            1 | 5 => {
                self.pending.extend_from_slice(nal);
                let access_unit = AccessUnit {
                    data: std::mem::take(&mut self.pending),
                    is_keyframe: nal_type == 5,
                };
                Some(access_unit)
            }
            _ => {
                self.pending.extend_from_slice(nal);
                None
            }
        }
    }
}

struct AnnexBStreamParser {
    buffer: Vec<u8>,
    builder: AccessUnitBuilder,
}

impl AnnexBStreamParser {
    fn new() -> Self {
        Self { buffer: Vec::new(), builder: AccessUnitBuilder::default() }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<AccessUnit> {
        self.buffer.extend_from_slice(chunk);
        let nals = drain_nals(&mut self.buffer);
        let mut access_units = Vec::new();
        for nal in nals {
            if let Some(unit) = self.builder.push_nal(&nal) {
                access_units.push(unit);
            }
        }
        access_units
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let cli = Cli::parse();

    let url = cli
        .url
        .or_else(|| env::var("LIVEKIT_URL").ok())
        .expect("Provide --url or set LIVEKIT_URL");
    let token = cli
        .token
        .or_else(|| env::var("LIVEKIT_TOKEN").ok())
        .expect("Provide --token or set LIVEKIT_TOKEN");

    match cli.command {
        Command::File { path } => {
            run_file(path, cli.width, cli.height, cli.fps, url, token).await?;
        }
        Command::Tcp { host, port } => {
            run_tcp(host, port, cli.width, cli.height, cli.fps, url, token).await?;
        }
    }

    Ok(())
}

async fn run_file(
    path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    url: String,
    token: String,
) -> Result<(), Box<dyn Error>> {
    let (room, source) = connect_and_publish(&url, &token, width, height).await?;

    let data = tokio::fs::read(&path).await?;
    info!("Read {} bytes from {}", data.len(), path.display());

    let nals = split_annex_b(&data);
    info!("Parsed {} NAL units", nals.len());

    let mut builder = AccessUnitBuilder::default();
    let mut access_units = Vec::new();
    for nal in nals {
        if let Some(unit) = builder.push_nal(&nal) {
            access_units.push(unit);
        }
    }

    if access_units.is_empty() {
        return Err("No H.264 access units found".into());
    }

    let frame_interval_us = frame_interval_us(fps);
    let base_ts = base_timestamp_us();

    for (idx, unit) in access_units.iter().enumerate() {
        let timestamp_us = base_ts + (idx as i64 * frame_interval_us);
        source.push_encoded_frame(&unit.data, timestamp_us, unit.is_keyframe);
    }

    info!("Published {} access units (no pacing)", access_units.len());
    info!("Press Ctrl-C to exit");
    tokio::signal::ctrl_c().await?;
    room.close().await?;

    Ok(())
}

async fn run_tcp(
    host: String,
    port: u16,
    width: u32,
    height: u32,
    fps: u32,
    url: String,
    token: String,
) -> Result<(), Box<dyn Error>> {
    let (room, source) = connect_and_publish(&url, &token, width, height).await?;

    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr).await?;
    info!("Connected to TCP source {}", addr);

    let frame_interval_us = frame_interval_us(fps);
    let base_ts = base_timestamp_us();
    let mut frame_idx: i64 = 0;
    let mut parser = AnnexBStreamParser::new();

    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            read_res = stream.read(&mut buf) => {
                let n = read_res?;
                if n == 0 {
                    warn!("TCP stream closed");
                    break;
                }
                for unit in parser.push(&buf[..n]) {
                    let timestamp_us = base_ts + (frame_idx * frame_interval_us);
                    source.push_encoded_frame(&unit.data, timestamp_us, unit.is_keyframe);
                    frame_idx += 1;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    room.close().await?;
    Ok(())
}

async fn connect_and_publish(
    url: &str,
    token: &str,
    width: u32,
    height: u32,
) -> Result<(Room, EncodedH264VideoSource), Box<dyn Error>> {
    let (room, mut rx) = Room::connect(url, token, RoomOptions::default()).await?;
    info!("Connected to room: {}", room.name());

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            info!("Room event: {:?}", event);
        }
    });

    let resolution = VideoResolution { width, height };
    let encoded_source = EncodedH264VideoSource::new(resolution);
    let source = RtcVideoSource::EncodedH264(encoded_source.clone());
    let track = LocalVideoTrack::create_video_track("h264", source);

    let publish_options = TrackPublishOptions {
        source: TrackSource::Camera,
        video_codec: VideoCodec::H264,
        simulcast: false,
        ..Default::default()
    };

    room.local_participant()
        .publish_track(LocalTrack::Video(track), publish_options)
        .await?;

    info!("Published H.264 video track");
    Ok((room, encoded_source))
}

fn base_timestamp_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
}

fn frame_interval_us(fps: u32) -> i64 {
    1_000_000i64 / fps as i64
}

fn start_code_len(data: &[u8], i: usize) -> Option<usize> {
    if i + 3 >= data.len() {
        return None;
    }
    if data[i] == 0 && data[i + 1] == 0 {
        if data[i + 2] == 1 {
            return Some(3);
        }
        if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
            return Some(4);
        }
    }
    None
}

fn find_start_codes(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if let Some(len) = start_code_len(data, i) {
            starts.push(i);
            i += len;
        } else {
            i += 1;
        }
    }
    starts
}

fn split_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let starts = find_start_codes(data);
    if starts.is_empty() {
        return Vec::new();
    }
    let mut nals = Vec::new();
    for idx in 0..starts.len() {
        let start = starts[idx];
        let end = if idx + 1 < starts.len() { starts[idx + 1] } else { data.len() };
        if end > start {
            nals.push(data[start..end].to_vec());
        }
    }
    nals
}

fn drain_nals(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let starts = find_start_codes(buffer);
    if starts.len() < 2 {
        return Vec::new();
    }

    let mut nals = Vec::new();
    for window in starts.windows(2) {
        let start = window[0];
        let end = window[1];
        if end > start {
            nals.push(buffer[start..end].to_vec());
        }
    }

    let tail_start = *starts.last().unwrap();
    let tail = buffer[tail_start..].to_vec();
    buffer.clear();
    buffer.extend_from_slice(&tail);

    nals
}

fn nal_type(nal: &[u8]) -> Option<u8> {
    let len = start_code_len(nal, 0)?;
    let idx = len;
    if idx >= nal.len() {
        return None;
    }
    Some(nal[idx] & 0x1F)
}
