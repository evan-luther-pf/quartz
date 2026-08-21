use std::{env, fs, path::PathBuf};

fn main() {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../modules");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("fixtures");
    fs::create_dir_all(&output).unwrap();
    println!("cargo:rerun-if-changed={}", source.display());

    let mut modules: Vec<_> = fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "wat" || extension == "wasm")
        })
        .collect();
    modules.sort();
    for wat_path in modules {
        let manifest_path = wat_path.with_extension("json");
        let manifest = fs::read(&manifest_path).unwrap();
        serde_json::from_slice::<serde_json::Value>(&manifest).unwrap();
        let mut component = if wat_path
            .extension()
            .is_some_and(|extension| extension == "wat")
        {
            wat::parse_file(&wat_path).unwrap()
        } else {
            fs::read(&wat_path).unwrap()
        };
        append_custom_section(&mut component, b"quartz:manifest", &manifest);
        let output_path = output
            .join(wat_path.file_stem().unwrap())
            .with_extension("wasm");
        fs::write(output_path, component).unwrap();
    }
    println!("cargo:rustc-env=QUARTZ_FIXTURE_DIR={}", output.display());
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
