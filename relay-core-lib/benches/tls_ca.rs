use criterion::{Criterion, black_box, criterion_group, criterion_main};
use relay_core_lib::tls::CertificateAuthority;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn bench_tls_ca(c: &mut Criterion) {
    init_crypto();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ca = rt.block_on(async { CertificateAuthority::new().unwrap() });

    let mut group = c.benchmark_group("tls/cert_gen");

    // Cold: generate certificate for a unique domain each time (forces cold path)
    group.sample_size(20);
    group.bench_function("cold_generation", |b| {
        let cold_counter = AtomicUsize::new(0);
        b.iter(|| {
            let i = cold_counter.fetch_add(1, Ordering::Relaxed);
            let domain = format!("cold-{}.internal", i);
            let ca = ca.clone();
            let _ = rt.block_on(async { ca.gen_server_config(black_box(&domain)).await });
        });
    });

    // Cache hit: lookup a pre-cached domain repeatedly
    let cached_domain = "cached.internal";
    rt.block_on(async {
        ca.gen_server_config(cached_domain).await.unwrap();
    });

    group.sample_size(200);
    group.bench_function("cache_hit", |b| {
        b.iter(|| {
            let ca = ca.clone();
            let _ = rt.block_on(async { ca.gen_server_config(black_box(cached_domain)).await });
        });
    });

    // Cache hit across many distinct cached domains
    let n_domains = 20;
    let domains: Vec<String> = (0..n_domains)
        .map(|i| format!("host{}.internal", i))
        .collect();
    rt.block_on(async {
        for d in &domains {
            ca.gen_server_config(d).await.unwrap();
        }
    });

    group.sample_size(100);
    group.bench_function("cache_hit_varied_20_domains", |b| {
        let lookup_counter = AtomicUsize::new(0);
        b.iter(|| {
            let i = lookup_counter.fetch_add(1, Ordering::Relaxed) % n_domains;
            let ca = ca.clone();
            let _ = rt.block_on(async { ca.gen_server_config(black_box(&domains[i])).await });
        });
    });

    group.finish();
}

criterion_group!(benches, bench_tls_ca);
criterion_main!(benches);
