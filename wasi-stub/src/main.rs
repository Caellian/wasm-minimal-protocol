mod parse_args;
mod parser_to_encoder;

use self::parser_to_encoder::ParserToEncoder;
use std::path::PathBuf;
use wasmparser::{Import, Parser, Payload, Type, TypeRef};

// Error handling
struct Error(Box<dyn std::fmt::Display>);
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl<E: std::fmt::Display + 'static> From<E> for Error {
    fn from(err: E) -> Self {
        Self(Box::new(err))
    }
}

fn main() -> Result<(), Error> {
    let parse_args::Args {
        binary,
        path,
        output_path,
        list,
    } = parse_args::Args::new(std::env::args_os().skip(1))?;
    let parser = Parser::default();
    if wasmparser::validate(&binary).is_err() {
        return Err("Error: the given wasm binary is invalid".into());
    }
    let payloads = parser.parse_all(&binary).collect::<Result<Vec<_>, _>>()?;

    let output = process_payloads(&binary, &payloads)?;

    if !list {
        write_output(path, output_path, output)?;
    } else {
        println!("NOTE: no output produced because the '--list' option was specified")
    }

    Ok(())
}

fn process_payloads(binary: &[u8], payloads: &[Payload]) -> Result<Vec<u8>, Error> {
    let mut result = wasm_encoder::Module::new();
    let mut types: Vec<Type> = Vec::new();
    let mut to_stub: Vec<Import> = Vec::new();
    let mut code_section = wasm_encoder::CodeSection::new();
    let mut in_code_section = false;
    let mut after_wasi = 0;
    // let mut before_wasi = 0; // TODO

    for payload in payloads {
        match payload {
            Payload::TypeSection(type_section) => {
                types = type_section
                    .clone()
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                let (id, range) = payload.as_section().unwrap();
                result.section(&wasm_encoder::RawSection {
                    id,
                    data: &binary[range],
                });
            }
            Payload::ImportSection(import_section) => {
                let mut imports = wasm_encoder::ImportSection::new();
                let mut after_wasi_count = None;
                for import in import_section.clone() {
                    let import = import?;
                    if import.module == "wasi_snapshot_preview1" {
                        after_wasi_count = Some(0);
                        to_stub.push(import);
                    } else {
                        if let Some(n) = after_wasi_count.as_mut() {
                            *n += 1;
                        } else {
                            // before_wasi += 1;  // TODO
                        }
                        imports.import(import.module, import.name, import.ty.convert());
                    }
                }
                after_wasi = after_wasi_count.unwrap_or(0);
                result.section(&imports);
            }
            Payload::FunctionSection(f) => {
                let mut functions_section = wasm_encoder::FunctionSection::new();
                for f in &to_stub {
                    let TypeRef::Func(ty) = f.ty else { continue };
                    functions_section.function(ty);
                }
                for f in f.clone() {
                    functions_section.function(f?);
                }
                result.section(&functions_section);
            }
            Payload::CodeSectionStart { .. } => {
                // TODO: reorder the 'call' instructions in all other functions !
                if after_wasi > 0 {
                    panic!("this crate cannot handle 'wasi_preview' imports that happen after other imports")
                }
                for f in &to_stub {
                    println!("found {}::{}: stubbing...", f.module, f.name);
                    let TypeRef::Func(ty) = f.ty else { continue };
                    let Type::Func(function_type) = &types[ty as usize] else { continue };
                    let locals = function_type
                        .params()
                        .iter()
                        .map(|t| (1u32, t.convert()))
                        .collect::<Vec<_>>();

                    let mut function = wasm_encoder::Function::new(locals);
                    if function_type.results().is_empty() {
                        function.instruction(&wasm_encoder::Instruction::End);
                    } else {
                        function.instruction(&wasm_encoder::Instruction::I32Const(76));
                        function.instruction(&wasm_encoder::Instruction::End);
                    }
                    code_section.function(&function);
                }
                in_code_section = true;
            }
            Payload::CodeSectionEntry(function_body) => {
                code_section.raw(&binary[function_body.range()]);
            }
            _ => {
                if in_code_section {
                    result.section(&code_section);
                    in_code_section = false;
                }
                if let Some((id, range)) = payload.as_section() {
                    result.section(&wasm_encoder::RawSection {
                        id,
                        data: &binary[range],
                    });
                }
            }
        };
    }
    let result = result.finish();
    wasmparser::validate(&result)?;
    Ok(result)
}

fn write_output(path: PathBuf, output_path: Option<PathBuf>, output: Vec<u8>) -> Result<(), Error> {
    let output_path = match output_path {
        Some(p) => p,
        // Try to find an unused output path
        None => {
            let mut i = 0;
            let mut file_name = path.file_stem().unwrap().to_owned();
            file_name.push(" - stubbed.wasm");
            loop {
                let mut new_path = path.clone();
                if i > 0 {
                    let mut file_name = path.file_stem().unwrap().to_owned();
                    file_name.push(format!(" - stubbed ({i}).wasm"));
                    new_path.set_file_name(&file_name);
                } else {
                    new_path.set_file_name(&file_name);
                }
                if !new_path.exists() {
                    break new_path;
                }
                i += 1;
            }
        }
    };
    std::fs::write(&output_path, output)?;
    let permissions = std::fs::File::open(path)?.metadata()?.permissions();
    std::fs::File::open(output_path)?.set_permissions(permissions)?;
    Ok(())
}
