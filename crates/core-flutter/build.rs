//! Compiles `tools/simhelper/simhelper.m` into `shame-sim`, the headless iOS Simulator
//! input helper (tap / swipe / Home). Best effort: on non-macOS hosts or without Xcode the
//! helper is simply absent and the editor reports that taps are unavailable.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../tools/simhelper/simhelper.m");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let src = PathBuf::from("../../tools/simhelper/simhelper.m");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("shame-sim");
    let status = Command::new("xcrun")
        .args(["--sdk", "macosx", "clang", "-fobjc-arc", "-O2"])
        .args(["-framework", "Foundation", "-framework", "CoreGraphics", "-framework", "IOSurface"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-env=SHAME_SIM_HELPER={}", out.display()),
        _ => println!("cargo:warning=could not build shame-sim (needs Xcode); simulator taps disabled"),
    }
}
