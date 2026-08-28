//! Evidence probe for indexing one real IAR ELF/DWARF artifact.

use std::{env, path::PathBuf};

use jlink_mcp::symbols::SymbolIndex;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or("用法：t_p2_dwarf <ELF/AXF/OUT 路径>")?,
    );
    let index = SymbolIndex::from_elf_path(&path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "path": path,
            "elf_sha256": index.elf_sha256(),
            "dwarf_versions": index.dwarf_versions(),
            "producers": index.producers(),
            "unit_count": index.unit_count(),
            "type_unit_count": index.type_unit_count(),
            "type_count": index.type_count(),
            "signature_reference_count": index.signature_reference_count(),
            "variable_definition_count": index.variable_definition_count(),
            "direct_path_count": index.direct_path_count(),
            "parser_format_version": jlink_domain::ACCESS_PLAN_FORMAT_VERSION
        }))?,
    );
    Ok(())
}
