use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../modules");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("fixtures");
    if output.exists() {
        fs::remove_dir_all(&output).unwrap();
    }
    fs::create_dir_all(&output).unwrap();
    println!("cargo:rerun-if-changed={}", source.display());
    build_repository_task(&output);

    let mut modules: Vec<_> = fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "wat"))
        .collect();
    modules.sort();
    for wat_path in modules {
        let manifest_path = wat_path.with_extension("json");
        let manifest = fs::read(&manifest_path).unwrap();
        serde_json::from_slice::<serde_json::Value>(&manifest).unwrap();
        let mut component = wat::parse_file(&wat_path).unwrap();
        append_custom_section(&mut component, b"quartz:manifest", &manifest);
        let output_path = output
            .join(wat_path.file_stem().unwrap())
            .with_extension("wasm");
        fs::write(output_path, component).unwrap();
    }
    let profile_components = PathBuf::from(env::var_os("OUT_DIR").unwrap())
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is below target profile directory")
        .join("components");
    if profile_components.exists() {
        fs::remove_dir_all(&profile_components).unwrap();
    }
    fs::create_dir_all(&profile_components).unwrap();
    for artifact in fs::read_dir(&output).unwrap() {
        let artifact = artifact.unwrap().path();
        if artifact
            .extension()
            .is_some_and(|extension| extension == "wasm")
        {
            fs::copy(
                &artifact,
                profile_components.join(artifact.file_name().unwrap()),
            )
            .unwrap();
        }
    }
    println!("cargo:rustc-env=QUARTZ_FIXTURE_DIR={}", output.display());
}

fn build_repository_task(output: &Path) {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let component = root.join("components/repository-task");
    println!(
        "cargo:rerun-if-changed={}",
        component.join("src/lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        component.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("wit/quartz-component.wit").display()
    );

    let rustc = Command::new("rustup")
        .args(["which", "rustc", "--toolchain", "stable"])
        .output()
        .expect("locate stable rustc");
    assert!(
        rustc.status.success(),
        "rustup could not locate stable rustc"
    );
    let rustc = String::from_utf8(rustc.stdout).expect("rustc path is UTF-8");
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("component-target");
    let status = Command::new("cargo")
        .args([
            "build",
            "--manifest-path",
            component.join("Cargo.toml").to_str().unwrap(),
            "--target",
            "wasm32-wasip2",
            "--release",
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .env("RUSTC", rustc.trim())
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .expect("build repository-task component");
    assert!(status.success(), "repository-task component build failed");

    let wasm = fs::read(
        target
            .join("wasm32-wasip2/release")
            .join("quartz_repository_task.wasm"),
    )
    .expect("read repository-task component");
    for name in [
        "repository-task-a",
        "repository-task-b",
        "repository-model-provider",
        "repository-terminal-provider",
        "repository-command-provider",
        "repository-approval-authority",
    ] {
        let manifest = fs::read(root.join("modules").join(format!("{name}.json")))
            .expect("read repository-task manifest");
        serde_json::from_slice::<serde_json::Value>(&manifest)
            .expect("parse repository-task manifest");
        let mut artifact = wasm.clone();
        append_custom_section(&mut artifact, b"quartz:manifest", &manifest);
        fs::write(output.join(name).with_extension("wasm"), artifact)
            .expect("write repository-task artifact");
    }
}

fn append_custom_section(component: &mut Vec<u8>, name: &[u8], data: &[u8]) {
    let mut payload = Vec::with_capacity(name.len() + data.len() + 8);
    encode_u32(name.len() as u32, &mut payload);
    payload.extend_from_slice(name);
    payload.extend_from_slice(data);
    component.push(0);
    let mut length = Vec::new();
    encode_u32(payload.len() as u32, &mut length);
    component.extend_from_slice(&length);
    component.extend_from_slice(&payload);
}

fn encode_u32(mut value: u32, output: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}
