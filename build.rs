use std::env;

fn main() {
    let release = env::var("PROFILE").as_deref() == Ok("release");
    let build = env::var("COLLAB_BUILD_VERSION")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            assert!(!release, "release builds must use scripts/build-collab.sh");
            0
        });

    println!("cargo:rustc-env=COLLAB_VERSION=0.2.{build:04}");
    println!("cargo:rerun-if-env-changed=COLLAB_BUILD_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
}
