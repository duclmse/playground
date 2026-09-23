use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/lua_runtime/c_api_shim.c");
    println!("cargo:rerun-if-changed=include/lua.h");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let object = out.join("c_api_shim.o");
    let archive = out.join("libsol_c_api_shim.a");
    let compiler = env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .args(["-std=c11", "-fPIC", "-Iinclude", "-c"])
        .arg("src/lua_runtime/c_api_shim.c")
        .arg("-o")
        .arg(&object)
        .status()
        .expect("run C compiler for Lua C API shim");
    assert!(status.success(), "failed to compile Lua C API shim");

    let archiver = env::var_os("AR").unwrap_or_else(|| "ar".into());
    let status = Command::new(archiver)
        .arg("crus")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("run archiver for Lua C API shim");
    assert!(status.success(), "failed to archive Lua C API shim");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=sol_c_api_shim");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for symbol in [
        "luaL_error",
        "lua_pushfstring",
        "lua_pushvfstring",
        "lua_pushexternalstring",
        "luaL_alloc",
        "luaL_buffinit",
        "luaL_prepbuffsize",
        "luaL_buffinitsize",
        "luaL_addlstring",
        "luaL_addstring",
        "luaL_addvalue",
        "luaL_pushresult",
        "luaL_pushresultsize",
        "luaL_addgsub",
        "luaL_gsub",
    ] {
        if target_os == "macos" {
            println!(
                "cargo:rustc-cdylib-link-arg=-Wl,-exported_symbol,_{}",
                symbol
            );
        } else if target_os == "linux" {
            println!(
                "cargo:rustc-cdylib-link-arg=-Wl,--export-dynamic-symbol={}",
                symbol
            );
        }
    }
}
