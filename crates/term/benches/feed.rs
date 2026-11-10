//! `Terminal::feed` against alacritty_terminal (vte `ansi::Processor` +
//! `Term`) on the vtebench payloads and realistic output, at 160x48 with
//! 10,000 lines of scrollback.
//!
//! Each payload body is repeated to at least 1 MiB (like vtebench) and fed in
//! 64 KiB chunks to a terminal that has already consumed the setup bytes and
//! keeps its state across iterations, so the numbers are steady-state
//! throughput (scrollback full, style table warm).

#[path = "../tests/support/payloads.rs"]
mod payloads;

use std::hint::black_box;
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use term::Terminal;

const COLS: usize = 160;
const ROWS: usize = 48;
const HISTORY: usize = 10_000;
const MIN_BYTES: usize = 1 << 20;
const CHUNK: usize = 64 * 1024;

fn feed(c: &mut Criterion) {
    let mut all = payloads::vtebench(COLS, ROWS, 26);
    all.extend(payloads::realistic(COLS, ROWS, 20));
    for p in &all {
        let body = payloads::repeat_to(&p.body, MIN_BYTES);
        let mut g = c.benchmark_group(p.name);
        g.throughput(Throughput::BytesDecimal(body.len() as u64));

        let mut ours = Terminal::new(COLS as u16, ROWS as u16, HISTORY);
        ours.feed(&p.setup);
        g.bench_function("tapust", |b| {
            b.iter(|| {
                for chunk in body.chunks(CHUNK) {
                    ours.feed(black_box(chunk));
                }
                black_box(ours.take_replies());
            })
        });

        let config = Config { scrolling_history: HISTORY, ..Config::default() };
        let mut alac = Term::new(config, &TermSize::new(COLS, ROWS), VoidListener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        proc.advance(&mut alac, &p.setup);
        g.bench_function("alacritty", |b| {
            b.iter(|| {
                for chunk in body.chunks(CHUNK) {
                    proc.advance(&mut alac, black_box(chunk));
                }
            })
        });
        g.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = feed
}
criterion_main!(benches);
