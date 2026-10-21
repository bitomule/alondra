//! Cargo builds of alondrad (rust-analyzer, local `cargo` runs) embed `Info.plist` like the Bazel
//! build the release ships: macOS asks a launchd agent for Local Network access with the reason it
//! carries (TN3179).
fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{manifest}/Info.plist");
    }
}
