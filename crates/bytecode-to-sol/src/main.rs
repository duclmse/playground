use std::path::{Path, PathBuf};

use bytecode_to_sol::{lua, sol, Error};

const USAGE: &str = "usage:\n  bytecode-to-sol sol <instructions.bin> [-o output.sol] [--name function]\n  bytecode-to-sol lua <chunk.luac> [-o output.sol] [--luac /path/to/luac]\n  bytecode-to-sol lua-listing <listing.txt> [-o output.sol]";

fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()) {
        eprintln!("error: {error}\n\n{USAGE}");
        std::process::exit(1);
    }
}

fn run(arguments: Vec<String>) -> Result<(), Error> {
    let Some(mode) = arguments.first().map(String::as_str) else {
        return Err(Error::InvalidInput(USAGE.into()));
    };
    if matches!(mode, "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    let input = arguments
        .get(1)
        .ok_or_else(|| Error::InvalidInput("missing input path".into()))?;
    let mut output_path = None;
    let mut luac = PathBuf::from("luac");
    let mut function_name = "decompiled".to_string();
    let mut index = 2;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-o" | "--output" => {
                output_path = Some(PathBuf::from(value_after(&arguments, index)?));
                index += 2;
            }
            "--luac" if mode == "lua" => {
                luac = PathBuf::from(value_after(&arguments, index)?);
                index += 2;
            }
            "--name" if mode == "sol" => {
                function_name = value_after(&arguments, index)?.to_string();
                index += 2;
            }
            option => {
                return Err(Error::InvalidInput(format!("unknown option '{option}'")));
            }
        }
    }

    let source = match mode {
        "sol" => sol::decompile(&std::fs::read(input)?, &function_name)?,
        "lua" => lua::decompile_chunk(Path::new(input), &luac)?,
        "lua-listing" => {
            let listing = std::fs::read_to_string(input)?;
            lua::render(&lua::parse_listing(&listing)?)
        }
        other => return Err(Error::InvalidInput(format!("unknown input mode '{other}'"))),
    };
    if let Some(path) = output_path {
        std::fs::write(path, source)?;
    } else {
        print!("{source}");
    }
    Ok(())
}

fn value_after(arguments: &[String], index: usize) -> Result<&str, Error> {
    arguments
        .get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("{} requires a value", arguments[index])))
}
