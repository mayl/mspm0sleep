//! There is deliberately no `memory.x` in the crate root: the linker would find it there first
//! and ignore the one chosen below (that linked a C1104 image with 4 KB of RAM once).
//!
//! This build script copies the board's `memory-*.x` file from the crate root into
//! a directory where the linker can always find it at build time.
//! For many projects this is optional, as the linker always searches the
//! project root directory -- wherever `Cargo.toml` is. However, if you
//! are using a workspace or have a more complicated build setup, this
//! build script becomes required. Additionally, by requesting that
//! Cargo re-run the build script whenever `memory.x` is changed,
//! updating `memory.x` ensures a rebuild of the application with the
//! new memory settings.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Put `memory.x` in our output directory and ensure it's
    // on the linker search path.
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(if env::var_os("CARGO_FEATURE_C1104").is_some() {
            include_bytes!("memory-c1104.x").as_slice()
        } else {
            include_bytes!("memory-l1306.x").as_slice()
        })
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // By default, Cargo will re-run a build script whenever
    // any file in the project changes. By specifying `memory.x`
    // here, we ensure the build script is only re-run when
    // `memory.x` is changed.
    println!("cargo:rerun-if-changed=memory-l1306.x");
    println!("cargo:rerun-if-changed=memory-c1104.x");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
    // You must tell cargo to link interrupt groups if the rt feature is enabled.
    println!("cargo:rustc-link-arg-bins=-Tinterrupt_group.x");
}
