# relay-core-runtime

The main Rust API for embedding RelayCore. This crate owns the runtime state, proxy lifecycle, intercept rules, policy management, and event streams.

```toml
[dependencies]
relay-core-runtime = "0.1"
```

> The `relay-core` crate name was unavailable on crates.io. `relay-core-runtime` is the official main package.

## Quick Start

```rust
use relay_core_runtime::{CoreState, ProxyConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let state = Arc::new(CoreState::new(None).await);
    let config = ProxyConfig::from_app_data_dir("./proxy-data", 8080).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1000);
    state.spawn_proxy(config, tx, None).unwrap();
    while let Some(update) = rx.recv().await {
        println!("Flow update: {:?}", update);
    }
}
```

## License

MIT
