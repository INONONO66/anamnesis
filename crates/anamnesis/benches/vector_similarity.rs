use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

fn bench_vector_similarity(c: &mut Criterion) {
    let counts = [1_000usize, 5_000, 10_000];
    let dims = [3usize, 128, 768];

    let mut group = c.benchmark_group("vector_similarity");

    for &count in &counts {
        for &dim in &dims {
            group.bench_with_input(
                BenchmarkId::new("cosine", format!("{count}x{dim}")),
                &(count, dim),
                |b, &(count, dim)| {
                    let mut rng = StdRng::seed_from_u64(42);
                    let collection: Vec<Vec<f64>> = (0..count)
                        .map(|_| (0..dim).map(|_| rng.r#gen::<f64>()).collect())
                        .collect();
                    let query: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();

                    b.iter(|| {
                        let max_score = collection
                            .iter()
                            .map(|v| cosine_similarity(black_box(&query), black_box(v)))
                            .fold(f64::NEG_INFINITY, f64::max);
                        black_box(max_score);
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_vector_similarity);
criterion_main!(benches);
