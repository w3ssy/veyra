//! Bettet `veyra.manifest` in die EXE ein (nur MSVC-Linker, ohne zusätzliche Abhängigkeit).

fn main() {
    println!("cargo:rerun-if-changed=veyra.manifest");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("veyra.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    } else if windows {
        println!("cargo:warning=Kein MSVC-Linker: Manifest wird nicht eingebettet – Windows 11 24H2 braucht es!");
    }
}
