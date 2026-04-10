use criterion::black_box;
use criterion::criterion_group;
use criterion::criterion_main;

fn memory_usage_benchmark(c: &mut criterion::Criterion) {
    c.bench_function("memory_usage_test", |b| {
        b.iter(|| black_box(1 + 1));
    });
}

criterion_group!(benches, memory_usage_benchmark);
criterion_main!(benches);
