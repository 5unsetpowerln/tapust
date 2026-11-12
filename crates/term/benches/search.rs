//! `Terminal::search` over a full scrollback: 10,000 lines of history and a
//! 160x48 screen, every row filled with words to its last column.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use term::Terminal;

const COLS: u16 = 160;
const ROWS: u16 = 48;
const HISTORY: usize = 10_000;

fn filled() -> Terminal {
    let mut t = Terminal::new(COLS, ROWS, HISTORY);
    let words = ["lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit", "sed", "do"];
    for i in 0..HISTORY + usize::from(ROWS) {
        let mut row = format!("{i:05} ");
        let mut w = i;
        while row.len() < usize::from(COLS) {
            row.push_str(words[w % words.len()]);
            row.push(' ');
            w = w * 7 + 3;
        }
        if i % 100 == 0 {
            row.replace_range(40..46, "Needle");
        }
        row.truncate(usize::from(COLS));
        t.feed(row.as_bytes());
        t.feed(b"\r\n");
    }
    assert_eq!(t.history_len(), HISTORY);
    t
}

fn search(c: &mut Criterion) {
    let t = filled();
    // Line 0 left the scrollback.
    assert_eq!(t.search("needle").len(), 100);
    let mut g = c.benchmark_group("search");
    g.bench_function("rare", |b| b.iter(|| black_box(t.search(black_box("needle")))));
    g.bench_function("absent", |b| b.iter(|| black_box(t.search(black_box("absent")))));
    g.bench_function("frequent", |b| b.iter(|| black_box(t.search(black_box("ip")))));
    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = search
}
criterion_main!(benches);
