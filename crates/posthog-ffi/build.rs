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

    // Without this, the dylib's install name is its build path, and every app
    // linked against a downloaded copy would look for it there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libposthog_ffi.dylib");
    }
}
