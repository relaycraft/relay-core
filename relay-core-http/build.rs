fn main() {
    #[cfg(feature = "webui")]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let index = std::path::Path::new(&manifest_dir).join("embed/webui/index.html");
        if !index.is_file() {
            panic!(
                "Web UI embed assets missing at {}. Run: ./scripts/webui-build.sh",
                index.display()
            );
        }
        println!("cargo:rerun-if-changed={}", index.display());
        let embed_dir = std::path::Path::new(&manifest_dir).join("embed/webui");
        if let Ok(entries) = std::fs::read_dir(&embed_dir) {
            for entry in entries.flatten() {
                println!("cargo:rerun-if-changed={}", entry.path().display());
            }
        }
    }
}
