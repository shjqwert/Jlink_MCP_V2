//! Evidence probe for parsing one real symbol ELF with the T-P2-IMG implementation.

use std::{env, fs, path::PathBuf};

use jlink_domain::FirmwareImage;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or("用法：t_p2_img <ELF/AXF/OUT 路径>")?,
    );
    let data = fs::read(&path)?;
    let file_name = path
        .file_name()
        .ok_or("镜像路径缺少文件名")?
        .to_string_lossy();
    let image = FirmwareImage::parse(&file_name, &data, None)?;
    let identity = image.symbol_identity_plan()?;
    let segments = identity
        .segments()
        .iter()
        .map(|segment| {
            json!({
                "address": format!("0x{:08X}", segment.address()),
                "length": segment.length(),
                "sha256": segment.sha256()
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "format": image.format(),
            "elf_sha256": identity.elf_sha256(),
            "segments": segments
        }))?
    );
    Ok(())
}
