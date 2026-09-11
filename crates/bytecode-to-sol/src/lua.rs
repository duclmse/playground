//! Lua binary-chunk adapter and `luac -l -l` listing decompiler.

use std::path::Path;
use std::process::Command;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub pc: usize,
    pub source_line: String,
    pub opcode: String,
    pub operands: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub origin: String,
    pub param_count: usize,
    pub vararg: bool,
    pub instructions: Vec<Instruction>,
}

pub fn disassemble_chunk(path: &Path, luac: &Path) -> Result<String, Error> {
    let bytes = std::fs::read(path)?;
    if !bytes.starts_with(b"\x1bLua") {
        return Err(Error::InvalidInput(format!(
            "'{}' is not a Lua binary chunk (missing ESC Lua signature)",
            path.display()
        )));
    }
    let output = Command::new(luac)
        .args(["-l", "-l"])
        .arg(path)
        .output()
        .map_err(|error| {
            Error::Tool(format!(
                "failed to run '{}': {error}; install the luac version that produced the chunk",
                luac.display()
            ))
        })?;
    if !output.status.success() {
        return Err(Error::Tool(format!(
            "{} could not decode '{}': {}; use the same Lua version that produced the chunk",
            luac.display(),
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| Error::Tool("luac emitted non-UTF-8 listing text".into()))
}

pub fn decompile_chunk(path: &Path, luac: &Path) -> Result<String, Error> {
    let listing = disassemble_chunk(path, luac)?;
    let functions = parse_listing(&listing)?;
    Ok(render(&functions))
}

pub fn parse_listing(listing: &str) -> Result<Vec<Function>, Error> {
    let mut functions = Vec::new();
    let mut current: Option<Function> = None;
    for raw_line in listing.lines() {
        let line = raw_line.trim();
        if line.starts_with("main <") || line.starts_with("function <") {
            if let Some(function) = current.take() {
                functions.push(function);
            }
            let nested_index = functions
                .iter()
                .filter(|function| function.name != "lua_main")
                .count();
            let (kind, origin) = line.split_once(' ').unwrap_or(("function", line));
            current = Some(Function {
                name: if kind == "main" {
                    "lua_main".into()
                } else {
                    format!("lua_function_{nested_index}")
                },
                origin: origin
                    .split_once(" (")
                    .map_or(origin, |(origin, _)| origin)
                    .to_string(),
                param_count: 0,
                vararg: false,
                instructions: Vec::new(),
            });
            continue;
        }
        let Some(function) = current.as_mut() else {
            continue;
        };
        if function.instructions.is_empty() && line.contains(" param") && line.contains(" slot") {
            if let Some(first) = line.split_whitespace().next() {
                function.vararg = first.ends_with('+');
                function.param_count = first.trim_end_matches('+').parse().unwrap_or(0);
            }
            continue;
        }
        if let Some(instruction) = parse_instruction(line) {
            function.instructions.push(instruction);
        }
    }
    if let Some(function) = current {
        functions.push(function);
    }
    if functions.is_empty() {
        return Err(Error::InvalidInput(
            "input does not contain a recognizable `luac -l -l` function listing".into(),
        ));
    }
    Ok(functions)
}

fn parse_instruction(line: &str) -> Option<Instruction> {
    let (code, comment) = line
        .split_once(';')
        .map_or((line, None), |(code, comment)| {
            (code, Some(comment.trim().to_string()))
        });
    let mut fields = code.split_whitespace();
    let pc = fields.next()?.parse().ok()?;
    let source_field = fields.next()?;
    if !source_field.starts_with('[') || !source_field.ends_with(']') {
        return None;
    }
    let source_line = source_field.trim_matches(['[', ']']).to_string();
    let opcode = fields.next()?.to_string();
    let operands = fields.map(ToString::to_string).collect();
    Some(Instruction {
        pc,
        source_line,
        opcode,
        operands,
        comment,
    })
}

pub fn render(functions: &[Function]) -> String {
    let mut output = String::new();
    output.push_str("-- Decompiled from a Lua binary chunk through luac.\n");
    output.push_str(
        "-- Control flow, tables, upvalues, and multi-return remain annotated bytecode.\n\n",
    );
    for function in functions {
        output.push_str(&format!("-- {}\nfn {}(", function.origin, function.name));
        for parameter in 0..function.param_count {
            if parameter != 0 {
                output.push_str(", ");
            }
            output.push_str(&format!("r{parameter}: any"));
        }
        output.push_str("): any\n");
        if function.vararg {
            output.push_str("    -- original function accepts Lua varargs\n");
        }
        for instruction in &function.instructions {
            output.push_str(&format!(
                "    -- [{:04}] line {} {}{}\n",
                instruction.pc,
                instruction.source_line,
                instruction.opcode,
                instruction
                    .comment
                    .as_ref()
                    .map_or(String::new(), |comment| format!("; {comment}"))
            ));
            output.push_str("    ");
            output.push_str(&render_instruction(instruction));
            output.push('\n');
        }
        if !function
            .instructions
            .iter()
            .any(|instruction| instruction.opcode.starts_with("RETURN"))
        {
            output.push_str("    return nil\n");
        }
        output.push_str("end\n\n");
    }
    output
}

fn operand(instruction: &Instruction, index: usize) -> &str {
    instruction
        .operands
        .get(index)
        .map(String::as_str)
        .unwrap_or("?")
}

fn lua_binary(instruction: &Instruction, operator: &str) -> String {
    format!(
        "r{} = r{} {operator} r{}",
        operand(instruction, 0),
        operand(instruction, 1),
        operand(instruction, 2)
    )
}

fn render_instruction(instruction: &Instruction) -> String {
    let a = operand(instruction, 0);
    let b = operand(instruction, 1);
    match instruction.opcode.as_str() {
        "MOVE" => format!("r{a} = r{b}"),
        "LOADI" | "LOADF" => format!("r{a} = {b}"),
        "LOADTRUE" => format!("r{a} = true"),
        "LOADFALSE" => format!("r{a} = false"),
        "LOADNIL" => format!("r{a} = nil"),
        "LOADK" | "LOADKX" => format!(
            "r{a} = {}",
            instruction.comment.as_deref().unwrap_or("constant")
        ),
        "ADDI" => format!("r{a} = r{b} + {}", operand(instruction, 2)),
        "SHRI" => format!("r{a} = r{b} >> {}", operand(instruction, 2)),
        "SHLI" => format!("r{a} = {} << r{b}", operand(instruction, 2)),
        "ADD" | "ADDK" => lua_binary(instruction, "+"),
        "SUB" | "SUBK" => lua_binary(instruction, "-"),
        "MUL" | "MULK" => lua_binary(instruction, "*"),
        "DIV" | "DIVK" => lua_binary(instruction, "/"),
        "IDIV" | "IDIVK" => lua_binary(instruction, "//"),
        "MOD" | "MODK" => lua_binary(instruction, "%"),
        "POW" | "POWK" => lua_binary(instruction, "^"),
        "BAND" | "BANDK" => lua_binary(instruction, "&"),
        "BOR" | "BORK" => lua_binary(instruction, "|"),
        "BXOR" | "BXORK" => lua_binary(instruction, "~"),
        "SHL" => lua_binary(instruction, "<<"),
        "SHR" => lua_binary(instruction, ">>"),
        "UNM" => format!("r{a} = -r{b}"),
        "BNOT" => format!("r{a} = ~r{b}"),
        "NOT" => format!("r{a} = not r{b}"),
        "LEN" => format!("r{a} = #r{b}"),
        "CONCAT" => format!("r{a} = concat(r{a} .. r{b})"),
        "NEWTABLE" => format!("r{a} = {{}}"),
        "GETTABLE" | "GETI" | "GETFIELD" => {
            format!("r{a} = r{b}[{}]", operand(instruction, 2))
        }
        "SETTABLE" | "SETI" | "SETFIELD" => {
            format!("r{a}[{b}] = {}", operand(instruction, 2))
        }
        "CLOSURE" => format!("r{a} = lua_function_{b}"),
        "CALL" | "TAILCALL" => format!("r{a} = r{a}(...)"),
        "RETURN1" => format!("return r{a}"),
        "RETURN0" => "return nil".into(),
        "RETURN" => format!("return r{a} -- Lua multi-return count {b}"),
        "JMP" => format!("goto bytecode_target({a})"),
        opcode if opcode.starts_with("MMBIN") => {
            format!("-- {opcode} metamethod fallback for the previous operation")
        }
        opcode => format!("-- {opcode} {}", instruction.operands.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"
main <example.lua:0,0> (3 instructions at 0x1)
0+ params, 2 slots, 1 upvalue, 0 locals, 0 constants, 0 functions
	1	[1]	LOADI    	0 41
	2	[1]	ADDI     	1 0 1
	3	[1]	RETURN1  	1
constants (0) for 0x1:
"#;

    #[test]
    fn parses_and_renders_luac_listing() {
        let functions = parse_listing(LISTING).unwrap();
        assert_eq!(functions.len(), 1);
        assert!(functions[0].vararg);
        assert_eq!(functions[0].instructions[1].opcode, "ADDI");
        let output = render(&functions);
        assert!(output.contains("fn lua_main(): any"));
        assert!(output.contains("r1 = r0 + 1"));
        assert!(output.contains("return r1"));
    }

    #[test]
    fn rejects_non_listing_text() {
        assert!(parse_listing("not bytecode").is_err());
    }
}
