use criterion::black_box;
use criterion::criterion_group;
use criterion::criterion_main;

fn storage_benchmark(c: &mut criterion::Criterion) {
    c.bench_function("storage_operations", |b| {
        b.iter(|| black_box(1 + 1));
    });
}

criterion_group!(benches, storage_benchmark);
criterion_main!(benches);
