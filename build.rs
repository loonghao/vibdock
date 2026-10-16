//! Use the installed Microsoft Windows SDK to embed the supplied artwork.
//! No downloaded build helper or runtime image dependency is needed.
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/brand/vibdock.ico");
    println!("cargo:rerun-if-changed=assets/brand/vibdock-light.ico");
    println!("cargo:rerun-if-changed=assets/brand/vibdock-dark.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Desktop resources currently require the Microsoft Windows SDK (MSVC target)"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let rc = out.join("vibdock.rc");
    let res = out.join("vibdock.res");
    let body = ["vibdock.ico", "vibdock-light.ico", "vibdock-dark.ico"]
        .iter()
        .enumerate()
        .map(|(id, name)| {
            format!(
                "{} ICON \"{}\"\n",
                id + 1,
                root.join("assets/brand")
                    .join(name)
                    .display()
                    .to_string()
                    .replace('\\', "/")
            )
        })
        .collect::<String>();
    std::fs::write(&rc, body).unwrap();
    let tool = resource_compiler()
        .expect("Microsoft Windows SDK rc.exe is required to build the branded launcher");
    let status = Command::new(tool)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc)
        .status()
        .expect("could not start Windows SDK resource compiler");
    assert!(status.success(), "Windows icon resource compilation failed");
    println!(
        "cargo:rustc-link-arg-bin=vibdock-launcher={}",
        res.display()
    );
    println!("cargo:rustc-link-arg-bin=vibdock={}", res.display());
}

fn resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH").and_then(|value| {
        env::split_paths(&value)
            .map(|p| p.join("rc.exe"))
            .find(|p| p.is_file())
    }) {
        return Some(path);
    }
    let base = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
    let mut versions: Vec<_> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    versions.sort();
    versions.reverse();
    versions
        .into_iter()
        .map(|path| path.join("x64/rc.exe"))
        .find(|path| path.is_file())
}
