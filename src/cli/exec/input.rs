use std::io::{IsTerminal, Read};
use std::path::Path;

use apcore::{ErrorCode, ModuleError};
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 1_048_576;

pub(super) fn read(inline: Option<&str>, path: Option<&Path>) -> anyhow::Result<Value> {
    let bytes = match path {
        Some(path) if path == Path::new("-") => {
            anyhow::ensure!(
                !std::io::stdin().is_terminal(),
                invalid("Standard input is a terminal; pipe JSON or use --input instead")
            );
            read_bounded(std::io::stdin().lock())?
        }
        Some(path) => {
            let file = std::fs::File::open(path)
                .map_err(|error| invalid(format!("Cannot open module input file: {error}")))?;
            read_bounded(file)?
        }
        None => inline.unwrap_or("{}").as_bytes().to_vec(),
    };
    anyhow::ensure!(
        path.is_none() || bytes.len() <= MAX_INPUT_BYTES,
        invalid(format!(
            "JSON input exceeds the {MAX_INPUT_BYTES}-byte limit"
        ))
    );
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        invalid(format!(
            "Invalid JSON input at line {}, column {}: {}",
            error.line(),
            error.column(),
            error
        ))
    })?;
    anyhow::ensure!(value.is_object(), invalid("Input must be a JSON object"));
    Ok(value)
}

fn read_bounded(reader: impl Read) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(format!("Cannot read module JSON input: {error}")))?;
    Ok(bytes)
}

fn invalid(message: impl Into<String>) -> ModuleError {
    ModuleError::new(ErrorCode::GeneralInvalidInput, message)
        .with_ai_guidance("Provide one JSON object matching the advertised input schema.")
}
