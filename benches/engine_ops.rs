use criterion::black_box;
use criterion::criterion_group;
use criterion::criterion_main;

fn engine_ops_benchmark(c: &mut criterion::Criterion) {
    c.bench_function("engine_operations", |b| {
        b.iter(|| black_box(1 + 1));
    });
}

criterion_group!(benches, engine_ops_benchmark);
criterion_main!(benches);
