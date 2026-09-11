use std::process::Command;

fn temporary(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "bytecode_to_sol_{name}_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ))
}

#[test]
fn cli_decompiles_raw_sol_words() {
    let path = temporary("sol.bin");
    let load_true = 1_u32 | (1 << 16);
    let return_r0 = 60_u32;
    let mut bytes = Vec::new();
    bytes.extend(load_true.to_le_bytes());
    bytes.extend(return_r0.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_bytecode-to-sol"))
        .args(["sol", path.to_str().unwrap(), "--name", "answer"])
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(output.status.success());
    let source = String::from_utf8(output.stdout).unwrap();
    assert!(source.contains("fn answer(): any"), "{source}");
    assert!(source.contains("r0 = true"), "{source}");
    assert!(source.contains("return r0"), "{source}");
}

#[test]
fn cli_decompiles_saved_luac_listing() {
    let path = temporary("listing.txt");
    std::fs::write(
        &path,
        "main <sample.lua:0,0> (2 instructions at 0x1)\n0+ params, 1 slot, 0 upvalues, 0 locals, 0 constants, 0 functions\n\t1\t[1]\tLOADI\t0 42\n\t2\t[1]\tRETURN1\t0\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bytecode-to-sol"))
        .args(["lua-listing", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(output.status.success());
    let source = String::from_utf8(output.stdout).unwrap();
    assert!(source.contains("r0 = 42"), "{source}");
    assert!(source.contains("return r0"), "{source}");
}

#[test]
fn cli_rejects_source_text_as_a_lua_chunk() {
    let path = temporary("source.lua");
    std::fs::write(&path, "return 42\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bytecode-to-sol"))
        .args(["lua", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a Lua binary chunk"));
}
