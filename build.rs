use std::env;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let version_path = manifest_dir.join(".collab-build-version");
    let lock_path = manifest_dir.join(".collab-build-version.lock");

    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let lock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(lock_result, 0, "failed to lock Collab build version");

    let current = fs::read_to_string(&version_path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = current
        .checked_add(1)
        .expect("Collab build version overflow");

    let temporary_path =
        manifest_dir.join(format!(".collab-build-version.tmp.{}", std::process::id()));
    fs::write(&temporary_path, format!("{next}\n")).unwrap();
    fs::rename(&temporary_path, &version_path).unwrap();

    println!("cargo:rustc-env=COLLAB_VERSION=0.2.{next:04}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", version_path.display());
    println!("cargo:rerun-if-changed={}", lock_path.display());
}
