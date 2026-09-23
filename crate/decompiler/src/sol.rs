//! Decoder for `crates/sol/src/bytecode.rs` instruction words.

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Abc,
    Abx,
    Asbx,
    AbcTag,
}

#[derive(Debug, Clone, Copy)]
struct OpInfo {
    name: &'static str,
    format: Format,
}

const fn op(name: &'static str, format: Format) -> OpInfo {
    OpInfo { name, format }
}

// Keep this table in opcode-number order with `sol::bytecode::Op`. Unknown
// numbers are rejected instead of inheriting Sol's internal Return fallback.
const OPS: [OpInfo; 70] = [
    op("LoadK", Format::Abx),
    op("LoadBool", Format::Abc),
    op("Move", Format::Abc),
    op("NegI", Format::Abc),
    op("NegF", Format::Abc),
    op("Not", Format::Abc),
    op("IntToFloat", Format::Abc),
    op("AddI", Format::Abc),
    op("SubI", Format::Abc),
    op("MulI", Format::Abc),
    op("DivI", Format::Abc),
    op("ModI", Format::Abc),
    op("AddF", Format::Abc),
    op("SubF", Format::Abc),
    op("MulF", Format::Abc),
    op("DivF", Format::Abc),
    op("EqI", Format::Abc),
    op("NeI", Format::Abc),
    op("LtI", Format::Abc),
    op("LeI", Format::Abc),
    op("GtI", Format::Abc),
    op("GeI", Format::Abc),
    op("EqF", Format::Abc),
    op("NeF", Format::Abc),
    op("LtF", Format::Abc),
    op("LeF", Format::Abc),
    op("GtF", Format::Abc),
    op("GeF", Format::Abc),
    op("EqB", Format::Abc),
    op("NeB", Format::Abc),
    op("And", Format::Abc),
    op("Or", Format::Abc),
    op("Jump", Format::Asbx),
    op("JumpIfFalse", Format::Asbx),
    op("Len", Format::Abc),
    op("NewArrayI64", Format::Abc),
    op("NewArrayF64", Format::Abc),
    op("Index", Format::Abc),
    op("SetIndex", Format::Abc),
    op("StructAlloc", Format::Abx),
    op("GetField", Format::Abc),
    op("SetField", Format::Abc),
    op("Box", Format::AbcTag),
    op("Unbox", Format::AbcTag),
    op("Call", Format::Abc),
    op("FloorDivF", Format::Abc),
    op("ModF", Format::Abc),
    op("PowF", Format::Abc),
    op("BandI", Format::Abc),
    op("BorI", Format::Abc),
    op("BxorI", Format::Abc),
    op("ShlI", Format::Abc),
    op("ShrI", Format::Abc),
    op("TrapIfZero", Format::Abc),
    op("AddNoOverflow", Format::Abc),
    op("DynamicBinary", Format::Abc),
    op("DynamicCompare", Format::Abc),
    op("DynamicNeg", Format::Abc),
    op("DynamicTruth", Format::Abc),
    op("StringOrder", Format::Abc),
    op("Return", Format::Abc),
    op("LoadFunc", Format::Abc),
    op("CallIndirect", Format::Abc),
    op("NewMapI64", Format::Abc),
    op("MapGetI64", Format::Abc),
    op("MapSetI64", Format::Abc),
    op("MapNextI64", Format::Abc),
    op("MapKeyI64", Format::Abc),
    op("MapValueI64", Format::Abc),
    op("ArrayMapI64", Format::Abc),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub pc: usize,
    pub word: u32,
    pub opcode: u8,
    pub name: &'static str,
    pub format: Format,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub bx: u16,
    pub sbx: i16,
    /// Full type tag stored in the following word by Box and Unbox.
    pub tag: Option<u32>,
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Instruction>, Error> {
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::InvalidInput(format!(
            "Sol bytecode length {} is not a multiple of four bytes",
            bytes.len()
        )));
    }
    let words = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    decode_words(&words)
}

pub fn decode_words(words: &[u32]) -> Result<Vec<Instruction>, Error> {
    let mut instructions = Vec::new();
    let mut pc = 0;
    while pc < words.len() {
        let word = words[pc];
        let opcode = word as u8;
        let info = OPS.get(opcode as usize).ok_or_else(|| {
            Error::InvalidInput(format!("unknown Sol opcode {opcode} at word {pc}"))
        })?;
        let tag = if info.format == Format::AbcTag {
            Some(*words.get(pc + 1).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "{} at word {pc} is missing its tag word",
                    info.name
                ))
            })?)
        } else {
            None
        };
        instructions.push(Instruction {
            pc,
            word,
            opcode,
            name: info.name,
            format: info.format,
            a: (word >> 8) as u8,
            b: (word >> 16) as u8,
            c: (word >> 24) as u8,
            bx: (word >> 16) as u16,
            sbx: (word >> 16) as u16 as i16,
            tag,
        });
        pc += if tag.is_some() { 2 } else { 1 };
    }
    Ok(instructions)
}

pub fn decompile(bytes: &[u8], function_name: &str) -> Result<String, Error> {
    let instructions = decode(bytes)?;
    Ok(render(&instructions, function_name))
}

pub fn render(instructions: &[Instruction], function_name: &str) -> String {
    let mut output = String::new();
    output.push_str("-- Decompiled from raw Sol tier-0 instruction words.\n");
    output.push_str("-- K[n], function_n, field_n, and dynamic_op_n require container metadata.\n");
    output.push_str(&format!("fn {}(): any\n", sanitize(function_name)));
    for instruction in instructions {
        output.push_str(&format!(
            "    -- [{:04}] 0x{:08x} {}\n",
            instruction.pc, instruction.word, instruction.name
        ));
        output.push_str("    ");
        output.push_str(&render_instruction(instruction));
        output.push('\n');
    }
    if !instructions
        .iter()
        .any(|instruction| instruction.name == "Return")
    {
        output.push_str("    return nil -- no Return opcode was present\n");
    }
    output.push_str("end\n");
    output
}

fn sanitize(name: &str) -> String {
    let mut output = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() || output.as_bytes()[0].is_ascii_digit() {
        output.insert_str(0, "decompiled_");
    }
    output
}

fn binary(instruction: &Instruction, operator: &str) -> String {
    format!(
        "r{} = r{} {operator} r{}",
        instruction.a, instruction.b, instruction.c
    )
}

fn render_instruction(instruction: &Instruction) -> String {
    let a = instruction.a;
    let b = instruction.b;
    let c = instruction.c;
    match instruction.name {
        "LoadK" => format!("r{a} = K[{}]", instruction.bx),
        "LoadBool" => format!("r{a} = {}", b != 0),
        "Move" => format!("r{a} = r{b}"),
        "NegI" | "NegF" | "DynamicNeg" => format!("r{a} = -r{b}"),
        "Not" => format!("r{a} = not r{b}"),
        "IntToFloat" => format!("r{a} = r{b} as f64"),
        "AddI" | "AddF" | "AddNoOverflow" => binary(instruction, "+"),
        "SubI" | "SubF" => binary(instruction, "-"),
        "MulI" | "MulF" => binary(instruction, "*"),
        "DivI" | "DivF" => binary(instruction, "/"),
        "ModI" | "ModF" => binary(instruction, "%"),
        "FloorDivF" => binary(instruction, "//"),
        "PowF" => binary(instruction, "^"),
        "BandI" => binary(instruction, "&"),
        "BorI" => binary(instruction, "|"),
        "BxorI" => binary(instruction, "~"),
        "ShlI" => binary(instruction, "<<"),
        "ShrI" => binary(instruction, ">>"),
        "EqI" | "EqF" | "EqB" => binary(instruction, "=="),
        "NeI" | "NeF" | "NeB" => binary(instruction, "~="),
        "LtI" | "LtF" => binary(instruction, "<"),
        "LeI" | "LeF" => binary(instruction, "<="),
        "GtI" | "GtF" => binary(instruction, ">"),
        "GeI" | "GeF" => binary(instruction, ">="),
        "And" => binary(instruction, "and"),
        "Or" => binary(instruction, "or"),
        "Jump" => format!(
            "goto L{}",
            instruction.pc as isize + 1 + instruction.sbx as isize
        ),
        "JumpIfFalse" => format!(
            "if not r{a} then goto L{} end",
            instruction.pc as isize + 1 + instruction.sbx as isize
        ),
        "Len" => format!("r{a} = #r{b}"),
        "NewArrayI64" => format!("r{a} = new_array_i64(r{b})"),
        "NewArrayF64" => format!("r{a} = new_array_f64(r{b})"),
        "Index" | "MapGetI64" => format!("r{a} = r{b}[r{c}]"),
        "SetIndex" | "MapSetI64" => format!("r{a}[r{b}] = r{c}"),
        "StructAlloc" => format!("r{a} = alloc_struct({})", instruction.bx),
        "GetField" => format!("r{a} = r{b}.field_{c}"),
        "SetField" => format!("r{a}.field_{b} = r{c}"),
        "Box" => format!(
            "r{a} = box_tag(r{b}, {})",
            instruction.tag.unwrap_or_default()
        ),
        "Unbox" => format!(
            "r{a} = unbox_tag(r{b}, {})",
            instruction.tag.unwrap_or_default()
        ),
        "Call" => format!(
            "r{a} = function_{b}(r{} .. r{})",
            a + 1,
            a.saturating_add(c)
        ),
        "TrapIfZero" => format!("assert(r{a} ~= 0)"),
        "DynamicBinary" => format!("r{a} = dynamic_op_{c}(r{a}, r{b})"),
        "DynamicCompare" => format!("r{a} = dynamic_compare_{c}(r{a}, r{b})"),
        "DynamicTruth" => format!("r{a} = truth(r{b})"),
        "StringOrder" => format!("r{a} = string_order(r{b}, r{c})"),
        "Return" => format!("return r{a}"),
        "LoadFunc" => format!("r{a} = function_{b}"),
        "CallIndirect" => {
            format!("r{a} = r{b}(r{} .. r{})", a + 1, a.saturating_add(c))
        }
        "NewMapI64" => format!("r{a} = {{}} -- Map<i64, i64>"),
        "MapNextI64" => format!("r{a} = map_next(r{b}, r{c})"),
        "MapKeyI64" => format!("r{a} = map_key(r{b}, r{c})"),
        "MapValueI64" => format!("r{a} = map_value(r{b}, r{c})"),
        "ArrayMapI64" => format!("r{a} = map(r{b}, r{c})"),
        other => format!("-- {other} A={a} B={b} C={c}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(opcode: u8, a: u8, b: u8, c: u8) -> u32 {
        opcode as u32 | (a as u32) << 8 | (b as u32) << 16 | (c as u32) << 24
    }

    #[test]
    fn decodes_operands_and_following_type_tags() {
        let instructions = decode_words(&[
            word(1, 0, 1, 0),
            word(42, 1, 0, 0),
            0x1234_5678,
            word(60, 1, 0, 0),
        ])
        .unwrap();
        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[1].name, "Box");
        assert_eq!(instructions[1].tag, Some(0x1234_5678));
        assert_eq!(instructions[2].pc, 3);
    }

    #[test]
    fn rejects_unknown_and_truncated_instructions() {
        assert!(decode_words(&[70]).is_err());
        assert!(decode_words(&[word(43, 0, 1, 0)]).is_err());
        assert!(decode(&[0, 1, 2]).is_err());
    }

    #[test]
    fn renders_sol_style_register_operations() {
        let instructions = decode_words(&[word(7, 2, 0, 1), word(60, 2, 0, 0)]).unwrap();
        let output = render(&instructions, "answer");
        assert!(output.contains("fn answer(): any"));
        assert!(output.contains("r2 = r0 + r1"));
        assert!(output.contains("return r2"));
    }
}
