use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Keep upstream's build configuration private to the bundled engine. Cargo's
    // --all-features must not turn on its experimental modes or test harnesses.
    for cfg in [
        "host_shared_wal",
        "injected_yields",
        "clt_turso_tests",
        "nightly",
        "loom",
        "shuttle",
        "antithesis",
    ] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }
    println!("cargo:rustc-check-cfg=cfg(clt_turso_feature, values(any()))");
    // The unmodified upstream proc macros also emit these disabled feature checks.
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"allocation_metric\", \"stacker\"))");
    // This is the feature set previously selected by CLT's Turso 0.7.2 dependency.
    for feature in [
        "fs",
        "uuid",
        "time",
        "json",
        "series",
        "encryption",
        "percentile",
        "conn_raw_api",
        "io_uring",
        "experimental_win_iocp",
        "pure-rust-crypto",
    ] {
        println!("cargo:rustc-cfg=clt_turso_feature=\"{feature}\"");
    }

    let os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS");
    let families = env::var("CARGO_CFG_TARGET_FAMILY").expect("Cargo target family");
    let width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").expect("Cargo target pointer width");
    if (families.split(',').any(|family| family == "unix") || os == "windows") && width == "64" {
        println!("cargo:rustc-cfg=host_shared_wal");
    }

    // Turso's SQL version functions describe the engine, not the CLT package or
    // checkout. Retain its upstream revision/date and identify our local patch.
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(
        out.join("built.rs"),
        concat!(
            "pub const PKG_VERSION: &str = \"0.7.2-clt.1\";\n",
            "pub const BUILT_TIME_SQLITE: &str = \"2026-07-30 13:38:56\";\n",
            "pub const GIT_COMMIT_HASH: Option<&str> = ",
            "Some(\"046e9cbf67d22491e8ecc941ec2891b02a9f3cad\");\n",
        ),
    )
    .expect("write bundled engine version metadata");
}
