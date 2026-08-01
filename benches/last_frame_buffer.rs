//! Bench the LastFrameBuffer read / write paths.
//!
//! Pins the cost of (a) replacing the JPEG preview (called by the
//! preview poller every ~5 s) and (b) snapshotting it for the MQTT
//! preview publisher / RTSP placeholder. Reads dominate writes ~10:1
//! in a busy deployment, so a regression in the read path is more
//! costly than a write-side regression.

use bairelay::rtsp::buffer::{LastFrameBuffer, VideoBurst};
use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn jpeg_set_get(c: &mut Criterion) {
	let buf = LastFrameBuffer::new();
	let payload = Bytes::from(vec![0xffu8; 64 * 1024]); // typical 4K JPEG ≈ 1.5 MiB; 64 KiB sub-stream

	let mut group = c.benchmark_group("last_frame_buffer/jpeg");
	group.throughput(Throughput::Bytes(payload.len() as u64));

	group.bench_function("set_jpeg/64KiB", |b| {
		b.iter(|| {
			buf.set_jpeg(black_box(payload.clone()));
		});
	});

	// Pre-populate so the read isn't the empty fast-path.
	buf.set_jpeg(payload.clone());
	group.bench_function("jpeg/64KiB", |b| {
		b.iter(|| {
			let snap = buf.jpeg();
			black_box(snap);
		});
	});

	group.finish();
}

fn video_replace_snapshot(c: &mut Criterion) {
	let buf = LastFrameBuffer::new();
	// Representative I-frame: ~64 KiB. Exact NAL count doesn't matter
	// for the bench; we're measuring the lock + clone cost.
	let burst = VideoBurst {
		codec: bairelay::rtsp::codec::VideoCodec::H264,
		parameter_sets: vec![vec![0x67u8; 8], vec![0x68u8; 4]],
		iframe_nals: vec![vec![0x65u8; 16 * 1024]; 4],
		pframe_nals: vec![],
		captured_at: std::time::Instant::now(),
		captured_pts_90khz: 0,
	};

	let mut group = c.benchmark_group("last_frame_buffer/video");
	group.throughput(Throughput::Bytes(64 * 1024));

	group.bench_function("replace_video/64KiB", |b| {
		b.iter(|| {
			buf.replace_video(black_box(burst.clone()));
		});
	});

	buf.replace_video(burst.clone());
	group.bench_function("video_snapshot/64KiB", |b| {
		b.iter(|| {
			let snap = buf.video_snapshot();
			black_box(snap);
		});
	});

	group.finish();
}

criterion_group!(benches, jpeg_set_get, video_replace_snapshot);
criterion_main!(benches);
