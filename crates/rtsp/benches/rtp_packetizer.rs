//! Throughput benchmarks for the H.264 / H.265 RTP packetisers.
//!
//! Pins per-frame packetisation cost so a future change to the
//! fragmentation / start-code-scanning loop can't double the cost
//! silently. Run with `cargo bench -p bairelay_rtsp`.

use bairelay_rtsp::codec::{h264, h265};
use bairelay_rtsp::rtp::RtpCounters;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

const MTU: usize = 1400;

fn h264_single_packetize(c: &mut Criterion) {
	// Small NAL that fits in one packet — hits the `packetize_single`
	// fast path. Representative of a typical SEI / AUD / SPS.
	let nal = vec![0x67u8; 32];
	let mut group = c.benchmark_group("h264_packetize_single");
	group.throughput(Throughput::Bytes(nal.len() as u64));
	group.bench_function("32B", |b| {
		let mut counters = RtpCounters::fixed(0xdead_beef, 0);
		b.iter(|| {
			let pkt = h264::packetize_single(black_box(&nal), &mut counters, 90_000, true);
			black_box(pkt);
		});
	});
	group.finish();
}

fn h264_fu_a_packetize(c: &mut Criterion) {
	// 32 KiB I-slice → fragmented into ~24 FU-A packets.
	let mut nal = vec![0x65u8; 32 * 1024];
	nal[0] = 0x65; // IDR slice header
	let mut group = c.benchmark_group("h264_packetize_fu_a");
	group.throughput(Throughput::Bytes(nal.len() as u64));
	group.bench_function("32KiB", |b| {
		let mut counters = RtpCounters::fixed(0xdead_beef, 0);
		b.iter(|| {
			let pkts = h264::packetize_fu_a(black_box(&nal), &mut counters, 90_000, true, MTU);
			black_box(pkts);
		});
	});
	group.finish();
}

fn h265_single_packetize(c: &mut Criterion) {
	let nal = vec![0x40u8, 0x01, 0xff, 0xff]; // VPS-shaped stub
	let mut group = c.benchmark_group("h265_packetize_single");
	group.throughput(Throughput::Bytes(nal.len() as u64));
	group.bench_function("4B", |b| {
		let mut counters = RtpCounters::fixed(0xdead_beef, 0);
		b.iter(|| {
			let pkt = h265::packetize_single(black_box(&nal), &mut counters, 90_000, true);
			black_box(pkt);
		});
	});
	group.finish();
}

fn h265_fu_packetize(c: &mut Criterion) {
	// 64 KiB IRAP — Argus 4K main-stream keyframes are this big.
	let mut nal = vec![0x26u8, 0x01]; // IRAP (NUT=19), TID=1
	nal.extend(std::iter::repeat_n(0xa5u8, 64 * 1024));
	let mut group = c.benchmark_group("h265_packetize_fu");
	group.throughput(Throughput::Bytes(nal.len() as u64));
	group.bench_function("64KiB", |b| {
		let mut counters = RtpCounters::fixed(0xdead_beef, 0);
		b.iter(|| {
			let pkts = h265::packetize_fu(black_box(&nal), &mut counters, 90_000, true, MTU);
			black_box(pkts);
		});
	});
	group.finish();
}

criterion_group!(
	benches,
	h264_single_packetize,
	h264_fu_a_packetize,
	h265_single_packetize,
	h265_fu_packetize,
);
criterion_main!(benches);
