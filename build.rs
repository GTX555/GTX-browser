//! Compiles `resource.rc` (icon + version info) when `rc.exe` from the
//! Windows SDK is available. If it is not, the build continues without the
//! resource — a warning is emitted instead of failing, keeping the toolchain
//! requirements as small as possible.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=resource.rc");
    println!("cargo:rerun-if-changed=assets/gtx.ico");
    println!("cargo:rerun-if-changed=gtx.manifest");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let res = out_dir.join("gtx.res");

    let Some(rc) = find_rc_exe() else {
        println!("cargo:warning=rc.exe not found; building without icon/version resource");
        return;
    };

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("resource.rc")
        .status();

    match status {
        Ok(s) if s.success() && res.is_file() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        Ok(s) => println!(
            "cargo:warning=rc.exe exited with {s}; building without icon/version resource"
        ),
        Err(e) => println!("cargo:warning=failed to run rc.exe ({e}); building without resource"),
    }
}

/// Newest `rc.exe` under the Windows SDK, else whatever is on PATH.
fn find_rc_exe() -> Option<PathBuf> {
    const KITS: &str = r"C:\Program Files (x86)\Windows Kits\10\bin";
    let mut best: Option<(String, PathBuf)> = None;

    if let Ok(entries) = std::fs::read_dir(KITS) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.chars().all(|c| c.is_ascii_digit() || c == '.') {
                continue;
            }
            let candidate = entry.path().join("x64").join("rc.exe");
            if candidate.is_file() {
                let newer = best.as_ref().is_none_or(|(v, _)| name.as_str() > v.as_str());
                if newer {
                    best = Some((name, candidate));
                }
            }
        }
    }
    if best.is_some() {
        return best.map(|(_, p)| p);
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join("rc.exe"))
        .find(|p| p.is_file())
}
