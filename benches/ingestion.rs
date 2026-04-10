use criterion::black_box;
use criterion::criterion_group;
use criterion::criterion_main;

fn ingestion_benchmark(c: &mut criterion::Criterion) {
    c.bench_function("ingest_single_node", |b| {
        b.iter(|| black_box(1 + 1));
    });
}

criterion_group!(benches, ingestion_benchmark);
criterion_main!(benches);
