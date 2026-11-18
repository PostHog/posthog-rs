use std::path::Path;

fn main() {
    // The bindings ship from this repository, so they report the version of
    // the SDK they are built against rather than a version of their own.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).expect("read the posthog manifest");
    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("posthog manifest has a package version");
    println!("cargo:rustc-env=POSTHOG_SDK_VERSION={version}");
}
