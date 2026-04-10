use criterion::black_box;
use criterion::criterion_group;
use criterion::criterion_main;

fn graph_crud_benchmark(c: &mut criterion::Criterion) {
    c.bench_function("graph_crud_operations", |b| {
        b.iter(|| black_box(1 + 1));
    });
}

criterion_group!(benches, graph_crud_benchmark);
criterion_main!(benches);
