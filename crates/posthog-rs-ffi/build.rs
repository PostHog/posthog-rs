fn main() {
    // Without this, the dylib's install name is its build path, and every app
    // linked against a downloaded copy would look for it there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libposthog_ffi.dylib");
    }
}
