use appendlist::AppendList;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
    measurement::WallTime,
};
use elsa::FrozenVec;
use once_vec::OnceVec;

const SIZES: &[usize] = &[64, 1_024, 16_384];

trait BenchCollection {
    fn new() -> Self;
    fn push_value(&self, value: usize);
    fn len(&self) -> usize;
    fn get_value(&self, index: usize) -> usize;
    fn sum_values(&self) -> u64;
}

struct OnceVecBoxed(OnceVec<Box<usize>>);

struct AppendListBoxed(AppendList<Box<usize>>);

impl BenchCollection for OnceVec<usize> {
    fn new() -> Self {
        Self::default()
    }

    fn push_value(&self, value: usize) {
        let _ = self.push(value);
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn get_value(&self, index: usize) -> usize {
        *self.get(index).expect("index in range")
    }

    fn sum_values(&self) -> u64 {
        self.iter().map(|value| *value as u64).sum()
    }
}

impl BenchCollection for AppendList<usize> {
    fn new() -> Self {
        Self::new()
    }

    fn push_value(&self, value: usize) {
        self.push(value);
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn get_value(&self, index: usize) -> usize {
        self[index]
    }

    fn sum_values(&self) -> u64 {
        self.iter().map(|value| *value as u64).sum()
    }
}

impl BenchCollection for FrozenVec<Box<usize>> {
    fn new() -> Self {
        Self::new()
    }

    fn push_value(&self, value: usize) {
        self.push(Box::new(value));
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn get_value(&self, index: usize) -> usize {
        *self.get(index).expect("index in range")
    }

    fn sum_values(&self) -> u64 {
        self.iter().map(|value| *value as u64).sum()
    }
}

impl BenchCollection for OnceVecBoxed {
    fn new() -> Self {
        Self(OnceVec::default())
    }

    fn push_value(&self, value: usize) {
        let _ = self.0.push(Box::new(value));
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn get_value(&self, index: usize) -> usize {
        **self.0.get(index).expect("index in range")
    }

    fn sum_values(&self) -> u64 {
        self.0.iter().map(|value| **value as u64).sum()
    }
}

impl BenchCollection for AppendListBoxed {
    fn new() -> Self {
        Self(AppendList::new())
    }

    fn push_value(&self, value: usize) {
        self.0.push(Box::new(value));
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn get_value(&self, index: usize) -> usize {
        *self.0[index]
    }

    fn sum_values(&self) -> u64 {
        self.0.iter().map(|value| **value as u64).sum()
    }
}

fn fill_collection<C: BenchCollection>(len: usize) -> C {
    let collection = C::new();
    for value in 0..len {
        collection.push_value(value);
    }
    collection
}

fn access_pattern(len: usize) -> Vec<usize> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut indices = Vec::with_capacity(len);

    for _ in 0..len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        indices.push((state as usize) % len);
    }

    indices
}

fn bench_push_case<C: BenchCollection>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    size: usize,
) {
    group.bench_with_input(BenchmarkId::new(name, size), &size, |b, &size| {
        b.iter(|| {
            let collection = C::new();
            for value in 0..size {
                collection.push_value(black_box(value));
            }
            black_box(collection.len())
        });
    });
}

fn bench_get_case<C: BenchCollection>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    size: usize,
) {
    let collection = fill_collection::<C>(size);
    let indices = access_pattern(size);

    group.bench_with_input(BenchmarkId::new(name, size), &size, |b, &_size| {
        b.iter(|| {
            let mut sum = 0_u64;
            for &index in &indices {
                sum += black_box(collection.get_value(index) as u64);
            }
            black_box(sum)
        });
    });
}

fn bench_iter_case<C: BenchCollection>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    size: usize,
) {
    let collection = fill_collection::<C>(size);

    group.bench_with_input(BenchmarkId::new(name, size), &size, |b, &_size| {
        b.iter(|| black_box(collection.sum_values()));
    });
}

fn bench_push_native(c: &mut Criterion) {
    let mut group = c.benchmark_group("append/native");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_push_case::<OnceVec<usize>>(&mut group, "once_vec", size);
        bench_push_case::<AppendList<usize>>(&mut group, "appendlist", size);
        bench_push_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn bench_push_boxed(c: &mut Criterion) {
    let mut group = c.benchmark_group("append/boxed");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_push_case::<OnceVecBoxed>(&mut group, "once_vec", size);
        bench_push_case::<AppendListBoxed>(&mut group, "appendlist", size);
        bench_push_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn bench_get_native(c: &mut Criterion) {
    let mut group = c.benchmark_group("get/native");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_get_case::<OnceVec<usize>>(&mut group, "once_vec", size);
        bench_get_case::<AppendList<usize>>(&mut group, "appendlist", size);
        bench_get_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn bench_get_boxed(c: &mut Criterion) {
    let mut group = c.benchmark_group("get/boxed");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_get_case::<OnceVecBoxed>(&mut group, "once_vec", size);
        bench_get_case::<AppendListBoxed>(&mut group, "appendlist", size);
        bench_get_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn bench_iter_native(c: &mut Criterion) {
    let mut group = c.benchmark_group("iter/native");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_iter_case::<OnceVec<usize>>(&mut group, "once_vec", size);
        bench_iter_case::<AppendList<usize>>(&mut group, "appendlist", size);
        bench_iter_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn bench_iter_boxed(c: &mut Criterion) {
    let mut group = c.benchmark_group("iter/boxed");

    for &size in SIZES {
        group.throughput(Throughput::Elements(size as u64));
        bench_iter_case::<OnceVecBoxed>(&mut group, "once_vec", size);
        bench_iter_case::<AppendListBoxed>(&mut group, "appendlist", size);
        bench_iter_case::<FrozenVec<Box<usize>>>(&mut group, "elsa_frozen_vec", size);
    }

    group.finish();
}

fn comparison_benchmarks(c: &mut Criterion) {
    bench_push_native(c);
    bench_push_boxed(c);
    bench_get_native(c);
    bench_get_boxed(c);
    bench_iter_native(c);
    bench_iter_boxed(c);
}

criterion_group!(benches, comparison_benchmarks);
criterion_main!(benches);
