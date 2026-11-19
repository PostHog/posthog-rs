fn main() {
    // Give the shared library a name that doesn't depend on where it was built.
    // Without it, every app linked against a downloaded copy records the
    // library's build or unpack path and fails to start anywhere else.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libposthog_ffi.dylib");
        }
        Ok("linux") => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libposthog_ffi.so");
        }
        _ => {}
    }
}
