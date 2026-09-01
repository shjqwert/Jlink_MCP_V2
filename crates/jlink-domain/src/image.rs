use std::{fmt::Write as _, path::Path};

use object::{
    Object, ObjectSection, ObjectSymbol,
    elf::PT_LOAD,
    read::elf::{ElfFile, FileHeader, ProgramHeader},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{ErrorCode, JlinkError};

/// A supported firmware image representation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareFormat {
    /// An ELF image, including ELF content stored with an AXF or OUT extension.
    Elf,
    /// An Intel HEX image.
    IntelHex,
    /// A Motorola S-record image.
    SRecord,
    /// A raw binary image whose first target address is supplied by the caller.
    Bin,
}

/// One non-empty, contiguous load range from a parsed firmware image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareSegment {
    address: u64,
    data: Vec<u8>,
}

impl FirmwareSegment {
    /// Returns the first target address covered by this segment.
    #[must_use]
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns the exact bytes stored in target address order.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// A parsed firmware image with content identity and normalized load segments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareImage {
    format: FirmwareFormat,
    sha256: String,
    segments: Vec<FirmwareSegment>,
    has_dwarf: bool,
    identity_block: Option<FirmwareIdentityBlock>,
}

impl FirmwareImage {
    /// Parses one supported image without accessing a probe or target.
    ///
    /// `file_name` is used only to select non-self-describing text or BIN formats.
    /// Valid ELF content is recognized by its magic even when named AXF or OUT.
    /// `base_address` is required for BIN and rejected for every self-addressed format.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when the format, checksum, address, or
    /// BIN base-address contract is invalid.
    pub fn parse(
        file_name: &str,
        data: &[u8],
        base_address: Option<u64>,
    ) -> Result<Self, JlinkError> {
        let extension = Path::new(file_name)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if extension.as_deref() == Some("map") {
            return Err(value_invalid("MAP 文件不能作为固件或符号输入"));
        }
        if data.is_empty() {
            return Err(value_invalid("固件镜像不能为空"));
        }

        let detected = detect_format(extension.as_deref(), data)?;
        match (detected, base_address) {
            (FirmwareFormat::Bin, None) => {
                return Err(value_invalid(
                    "BIN 镜像必须在每次 flash/verify 请求中显式提供 base_address",
                ));
            }
            (FirmwareFormat::Bin, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(value_invalid(
                    "base_address 只允许用于 BIN；ELF、HEX 和 SREC 使用镜像自带地址",
                ));
            }
        }

        let (segments, has_dwarf, identity_block) = match detected {
            FirmwareFormat::Elf => parse_elf(data)?,
            FirmwareFormat::IntelHex => (parse_intel_hex(data)?, false, None),
            FirmwareFormat::SRecord => (parse_s_record(data)?, false, None),
            FirmwareFormat::Bin => {
                let Some(address) = base_address else {
                    return Err(value_invalid(
                        "BIN 镜像必须在每次 flash/verify 请求中显式提供 base_address",
                    ));
                };
                (vec![segment(address, data.to_vec())?], false, None)
            }
        };
        Ok(Self {
            format: detected,
            sha256: sha256(data),
            segments,
            has_dwarf,
            identity_block,
        })
    }

    /// Returns the detected image format.
    #[must_use]
    pub const fn format(&self) -> FirmwareFormat {
        self.format
    }

    /// Returns the lowercase SHA-256 digest of the complete source image.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns normalized non-overlapping load segments in ascending address order.
    #[must_use]
    pub fn segments(&self) -> &[FirmwareSegment] {
        &self.segments
    }

    /// Creates the read-only target identity plan for a symbol ELF.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] unless this image is ELF, contains
    /// DWARF information, and has at least one non-empty load segment.
    pub fn symbol_identity_plan(&self) -> Result<FirmwareIdentityPlan, JlinkError> {
        if self.format != FirmwareFormat::Elf {
            return Err(value_invalid("变量和 HSS 符号源必须是 ELF/AXF/OUT"));
        }
        if !self.has_dwarf {
            return Err(value_invalid("符号 ELF 不包含可用的 DWARF .debug_info"));
        }
        Ok(FirmwareIdentityPlan {
            elf_sha256: self.sha256.clone(),
            identity_block: self.identity_block.clone(),
            legacy_segments: Vec::new(),
        })
    }
}

/// The portable ELF symbol used for the bounded target firmware identity block.
pub const FIRMWARE_IDENTITY_SYMBOL: &str = "__jlink_mcp_identity";

/// Maximum number of target bytes read to prove one symbol ELF identity.
pub const MAX_FIRMWARE_IDENTITY_BYTES: usize = 64;

const FIRMWARE_IDENTITY_MAGIC: &[u8; 4] = b"JLID";
const FIRMWARE_IDENTITY_FORMAT_VERSION: u8 = 1;

/// Whether a symbol ELF provides a target-verifiable firmware identity block.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareIdentityStrength {
    /// The ELF has no identity symbol; only non-mutating DWARF reads may proceed with a warning.
    Weak,
    /// The ELF contains a valid identity block that can be compared with at most 64 target bytes.
    Strong,
}

/// One fixed, bounded target identity block extracted from the symbol ELF.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareIdentityBlock {
    address: u64,
    bytes: Vec<u8>,
    magic: String,
    format_version: u8,
    build_id: String,
}

impl FirmwareIdentityBlock {
    fn parse(address: u64, bytes: &[u8]) -> Result<Self, JlinkError> {
        if !(6..=MAX_FIRMWARE_IDENTITY_BYTES).contains(&bytes.len()) {
            return Err(value_invalid(format!(
                "{FIRMWARE_IDENTITY_SYMBOL} 大小必须在 6 到 {MAX_FIRMWARE_IDENTITY_BYTES} 字节之间"
            )));
        }
        if &bytes[..4] != FIRMWARE_IDENTITY_MAGIC {
            return Err(value_invalid(format!(
                "{FIRMWARE_IDENTITY_SYMBOL} magic 必须为 JLID"
            )));
        }
        if bytes[4] != FIRMWARE_IDENTITY_FORMAT_VERSION {
            return Err(value_invalid(format!(
                "{FIRMWARE_IDENTITY_SYMBOL} format_version {} 不受支持",
                bytes[4]
            )));
        }
        let build_id_length = usize::from(bytes[5]);
        if build_id_length == 0 || 6 + build_id_length > bytes.len() {
            return Err(value_invalid(format!(
                "{FIRMWARE_IDENTITY_SYMBOL} build_id 长度无效"
            )));
        }
        let build_id_bytes = &bytes[6..6 + build_id_length];
        if !build_id_bytes
            .iter()
            .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        {
            return Err(value_invalid(format!(
                "{FIRMWARE_IDENTITY_SYMBOL} build_id 必须是可打印 ASCII"
            )));
        }
        let length =
            u64::try_from(bytes.len()).map_err(|_| value_invalid("固件身份块长度无法表示"))?;
        address
            .checked_add(length)
            .ok_or_else(|| value_invalid("固件身份块地址溢出"))?;
        Ok(Self {
            address,
            bytes: bytes.to_vec(),
            magic: "JLID".to_owned(),
            format_version: FIRMWARE_IDENTITY_FORMAT_VERSION,
            build_id: String::from_utf8(build_id_bytes.to_vec())
                .expect("printable ASCII is valid UTF-8"),
        })
    }

    /// Returns the first target address of the identity block.
    #[must_use]
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns the exact identity block bytes expected on the target.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the parsed identity block format version.
    #[must_use]
    pub const fn format_version(&self) -> u8 {
        self.format_version
    }

    /// Returns the printable build identifier declared by the firmware.
    #[must_use]
    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    fn is_valid(&self) -> bool {
        Self::parse(self.address, &self.bytes).is_ok()
            && self.magic == "JLID"
            && self.format_version == FIRMWARE_IDENTITY_FORMAT_VERSION
            && self
                .bytes
                .get(6..6 + usize::from(self.bytes[5]))
                .is_some_and(|bytes| bytes == self.build_id.as_bytes())
    }
}

/// The expected identity of one ELF load segment or one target readback range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareSegmentFingerprint {
    address: u64,
    length: u64,
    sha256: String,
}

impl FirmwareSegmentFingerprint {
    /// Hashes a complete target readback range for identity comparison.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when the range is empty or overflows
    /// the target address width represented by `u64`.
    pub fn from_bytes(address: u64, data: &[u8]) -> Result<Self, JlinkError> {
        if data.is_empty() {
            return Err(value_invalid("固件身份读取范围不能为空"));
        }
        let length =
            u64::try_from(data.len()).map_err(|_| value_invalid("固件身份读取范围长度无法表示"))?;
        address
            .checked_add(length)
            .ok_or_else(|| value_invalid("固件身份读取范围地址溢出"))?;
        Ok(Self {
            address,
            length,
            sha256: sha256(data),
        })
    }

    /// Returns the first target address covered by this fingerprint.
    #[must_use]
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns the number of bytes covered by this fingerprint.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the lowercase SHA-256 digest of the covered bytes.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    fn is_valid(&self) -> bool {
        self.length > 0
            && self.address.checked_add(self.length).is_some()
            && is_sha256(&self.sha256)
    }
}

/// An immutable symbol-ELF identity plan verified against target Flash readbacks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareIdentityPlan {
    elf_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_block: Option<FirmwareIdentityBlock>,
    /// Accepted only to read V1.0 persisted capture requests. It is never used
    /// for implicit identity reads in V1.1.
    #[serde(default, rename = "segments", skip_serializing_if = "Vec::is_empty")]
    legacy_segments: Vec<FirmwareSegmentFingerprint>,
}

impl FirmwareIdentityPlan {
    /// Returns the SHA-256 digest of the complete symbol ELF.
    #[must_use]
    pub fn elf_sha256(&self) -> &str {
        &self.elf_sha256
    }

    /// Returns the exact target ranges that must be read without side effects.
    #[must_use]
    pub fn segments(&self) -> &[FirmwareSegmentFingerprint] {
        &self.legacy_segments
    }

    /// Returns the bounded identity block, when the ELF opted into strong identity.
    #[must_use]
    pub const fn identity_block(&self) -> Option<&FirmwareIdentityBlock> {
        self.identity_block.as_ref()
    }

    /// Returns whether this ELF has strong or weak firmware identity evidence.
    #[must_use]
    pub const fn strength(&self) -> FirmwareIdentityStrength {
        if self.identity_block.is_some() {
            FirmwareIdentityStrength::Strong
        } else {
            FirmwareIdentityStrength::Weak
        }
    }

    /// Rejects a mutating or symbol-capture operation unless strong identity exists.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FirmwareIdentityUnknown`] for legacy ELFs without the
    /// fixed identity symbol.
    pub fn ensure_strong(&self) -> Result<&FirmwareIdentityBlock, JlinkError> {
        self.validate()?;
        self.identity_block.as_ref().ok_or_else(|| {
            JlinkError::new(
                ErrorCode::FirmwareIdentityUnknown,
                format!(
                    "符号 ELF 缺少 {FIRMWARE_IDENTITY_SYMBOL}，变量写入和符号 HSS 要求强固件身份"
                ),
                false,
            )
            .with_detail("identity_strength", json!("weak"))
            .with_detail("required_symbol", json!(FIRMWARE_IDENTITY_SYMBOL))
            .with_detail("maximum_bytes", json!(MAX_FIRMWARE_IDENTITY_BYTES))
        })
    }

    /// Revalidates the immutable identity plan after local IPC transport.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FirmwareIdentityUnknown`] when the ELF digest or
    /// segment set cannot prove one exact target firmware identity.
    pub fn validate(&self) -> Result<(), JlinkError> {
        if self.is_valid() {
            Ok(())
        } else {
            Err(identity_unknown("符号 ELF 身份计划无效", None))
        }
    }

    /// Verifies complete target readback fingerprints against this ELF plan.
    ///
    /// `None`, a missing range, or duplicate evidence cannot prove identity and
    /// returns [`ErrorCode::FirmwareIdentityUnknown`]. An exact range with a
    /// different digest proves a mismatch and returns
    /// [`ErrorCode::FirmwareElfMismatch`]. Additional unrelated ranges are ignored.
    ///
    /// # Errors
    ///
    /// Returns the stable identity error described above. This function performs
    /// no target access and never changes target state.
    pub fn verify_target(
        &self,
        observed: Option<&[FirmwareSegmentFingerprint]>,
    ) -> Result<(), JlinkError> {
        self.validate()?;
        let Some(observed) = observed else {
            return Err(identity_unknown("目标 Flash 身份读取未完成", None));
        };
        if observed.iter().any(|fingerprint| !fingerprint.is_valid()) {
            return Err(identity_unknown("目标 Flash 身份读取证据无效", None));
        }
        if self.legacy_segments.is_empty() {
            return Err(identity_unknown(
                "V1.1 固件身份不再执行完整镜像读取，请验证固定身份块",
                None,
            ));
        }
        for expected in &self.legacy_segments {
            let mut matches = observed.iter().filter(|candidate| {
                candidate.address == expected.address && candidate.length == expected.length
            });
            let Some(actual) = matches.next() else {
                return Err(identity_unknown(
                    "目标 Flash 身份缺少完整读取范围",
                    Some(expected),
                ));
            };
            if matches.next().is_some() {
                return Err(identity_unknown(
                    "目标 Flash 身份包含重复读取范围",
                    Some(expected),
                ));
            }
            if actual.sha256 != expected.sha256 {
                return Err(JlinkError::new(
                    ErrorCode::FirmwareElfMismatch,
                    "目标 Flash 已确认与符号 ELF 不匹配",
                    false,
                )
                .with_detail("address", json!(format!("0x{:X}", expected.address)))
                .with_detail("length", json!(expected.length))
                .with_detail("expected_sha256", json!(expected.sha256))
                .with_detail("actual_sha256", json!(actual.sha256)));
            }
        }
        Ok(())
    }

    /// Compares the exact bounded target identity block with the symbol ELF.
    ///
    /// # Errors
    ///
    /// Returns a stable unknown or mismatch error without requesting additional
    /// target ranges.
    pub fn verify_target_bytes(&self, observed: Option<&[u8]>) -> Result<(), JlinkError> {
        let expected = self.ensure_strong()?;
        let Some(observed) = observed else {
            return Err(identity_unknown("目标固件身份块读取未完成", None));
        };
        if observed.len() != expected.bytes.len() {
            return Err(identity_unknown("目标固件身份块读取长度不完整", None)
                .with_detail("address", json!(format!("0x{:X}", expected.address)))
                .with_detail("expected_length", json!(expected.bytes.len()))
                .with_detail("actual_length", json!(observed.len())));
        }
        if observed != expected.bytes {
            return Err(JlinkError::new(
                ErrorCode::FirmwareElfMismatch,
                "目标固件身份块与符号 ELF 不匹配",
                false,
            )
            .with_detail("address", json!(format!("0x{:X}", expected.address)))
            .with_detail("length", json!(expected.bytes.len()))
            .with_detail("expected_build_id", json!(expected.build_id))
            .with_detail("expected_sha256", json!(sha256(&expected.bytes)))
            .with_detail("actual_sha256", json!(sha256(observed))));
        }
        Ok(())
    }

    fn is_valid(&self) -> bool {
        if !is_sha256(&self.elf_sha256) {
            return false;
        }
        if self
            .identity_block
            .as_ref()
            .is_some_and(|block| !block.is_valid())
        {
            return false;
        }
        let mut previous_end = None;
        for segment in &self.legacy_segments {
            if !segment.is_valid() || previous_end.is_some_and(|end| segment.address < end) {
                return false;
            }
            previous_end = segment.address.checked_add(segment.length);
        }
        true
    }
}

fn detect_format(extension: Option<&str>, data: &[u8]) -> Result<FirmwareFormat, JlinkError> {
    if data.starts_with(b"\x7fELF") {
        return Ok(FirmwareFormat::Elf);
    }
    match extension {
        Some("elf" | "axf" | "out") => {
            return Err(value_invalid("ELF/AXF/OUT 文件的内容不是有效 ELF"));
        }
        Some("bin") => return Ok(FirmwareFormat::Bin),
        Some("hex" | "ihex") => return Ok(FirmwareFormat::IntelHex),
        Some("srec" | "s19" | "s28" | "s37" | "mot") => {
            return Ok(FirmwareFormat::SRecord);
        }
        _ => {}
    }
    let text = std::str::from_utf8(data)
        .map_err(|_| value_invalid("无法根据内容识别固件格式，请使用受支持的扩展名"))?;
    let first = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| value_invalid("固件镜像不能为空"))?;
    if first.starts_with(':') {
        Ok(FirmwareFormat::IntelHex)
    } else if first.len() >= 2
        && first.as_bytes()[0].eq_ignore_ascii_case(&b'S')
        && first.as_bytes()[1].is_ascii_digit()
    {
        Ok(FirmwareFormat::SRecord)
    } else {
        Err(value_invalid("固件格式不受支持"))
    }
}

fn parse_elf(
    data: &[u8],
) -> Result<(Vec<FirmwareSegment>, bool, Option<FirmwareIdentityBlock>), JlinkError> {
    let file = object::File::parse(data)
        .map_err(|error| value_invalid(format!("ELF 结构无效：{error}")))?;
    let has_dwarf = [".debug_info", ".zdebug_info"].iter().any(|name| {
        file.section_by_name(name)
            .is_some_and(|section| section.size() > 0)
    });
    let segments = match &file {
        object::File::Elf32(file) => parse_elf_segments(file)?,
        object::File::Elf64(file) => parse_elf_segments(file)?,
        _ => return Err(value_invalid("镜像魔数不是 ELF")),
    };
    let identity_block = parse_elf_identity_block(&file)?;
    Ok((normalize_segments(segments)?, has_dwarf, identity_block))
}

fn parse_elf_identity_block(
    file: &object::File<'_>,
) -> Result<Option<FirmwareIdentityBlock>, JlinkError> {
    let mut matches = file.symbols().filter(|symbol| {
        symbol.is_definition() && symbol.name().ok() == Some(FIRMWARE_IDENTITY_SYMBOL)
    });
    let Some(symbol) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(value_invalid(format!(
            "ELF 包含重复的 {FIRMWARE_IDENTITY_SYMBOL} 定义"
        )));
    }
    let size = symbol.size();
    if !(6..=u64::try_from(MAX_FIRMWARE_IDENTITY_BYTES).expect("constant fits u64")).contains(&size)
    {
        return Err(value_invalid(format!(
            "{FIRMWARE_IDENTITY_SYMBOL} 大小必须在 6 到 {MAX_FIRMWARE_IDENTITY_BYTES} 字节之间"
        )));
    }
    let section_index = symbol.section_index().ok_or_else(|| {
        value_invalid(format!(
            "{FIRMWARE_IDENTITY_SYMBOL} 必须位于可加载 ELF section"
        ))
    })?;
    let section = file
        .section_by_index(section_index)
        .map_err(|error| value_invalid(format!("固件身份 section 无效：{error}")))?;
    let bytes = section
        .data_range(symbol.address(), size)
        .map_err(|error| value_invalid(format!("固件身份块超出 section：{error}")))?
        .ok_or_else(|| value_invalid("固件身份块没有可加载字节"))?;
    FirmwareIdentityBlock::parse(symbol.address(), bytes).map(Some)
}

fn parse_elf_segments<Elf>(file: &ElfFile<'_, Elf>) -> Result<Vec<FirmwareSegment>, JlinkError>
where
    Elf: FileHeader,
    Elf::Word: Into<u64>,
{
    let endian = file.endian();
    let mut segments = Vec::new();
    for header in file.elf_program_headers() {
        if header.p_type(endian) != PT_LOAD || header.p_filesz(endian).into() == 0 {
            continue;
        }
        let bytes = header
            .data(endian, file.data())
            .map_err(|()| value_invalid("ELF 可加载段超出文件边界"))?;
        segments.push(segment(header.p_paddr(endian).into(), bytes.to_vec())?);
    }
    if segments.is_empty() {
        return Err(value_invalid("ELF 不包含非空 PT_LOAD 段"));
    }
    Ok(segments)
}

fn parse_intel_hex(data: &[u8]) -> Result<Vec<FirmwareSegment>, JlinkError> {
    let text = std::str::from_utf8(data)
        .map_err(|_| value_invalid("Intel HEX 必须是 UTF-8/ASCII 文本"))?;
    let mut upper_address = 0_u64;
    let mut raw_segments = Vec::new();
    let mut saw_eof = false;
    for (index, untrimmed) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = untrimmed.trim();
        if line.is_empty() {
            continue;
        }
        if saw_eof {
            return Err(value_invalid(format!(
                "Intel HEX 第 {} 行位于 EOF 之后",
                index + 1
            )));
        }
        let body = line
            .strip_prefix(':')
            .ok_or_else(|| value_invalid(format!("Intel HEX 第 {} 行缺少 ':'", index + 1)))?;
        let record = decode_hex(body, "Intel HEX", index + 1)?;
        if record.len() < 5 || record.len() != usize::from(record[0]) + 5 {
            return Err(value_invalid(format!(
                "Intel HEX 第 {} 行记录长度无效",
                index + 1
            )));
        }
        if record.iter().copied().fold(0_u8, u8::wrapping_add) != 0 {
            return Err(value_invalid(format!(
                "Intel HEX 第 {} 行校验和无效",
                index + 1
            )));
        }
        let offset = u64::from(u16::from_be_bytes([record[1], record[2]]));
        let payload = &record[4..record.len() - 1];
        match record[3] {
            0x00 => {
                if !payload.is_empty() {
                    let address = upper_address
                        .checked_add(offset)
                        .ok_or_else(|| value_invalid("Intel HEX 地址溢出"))?;
                    raw_segments.push(segment(address, payload.to_vec())?);
                }
            }
            0x01 if payload.is_empty() => saw_eof = true,
            0x02 if payload.len() == 2 => {
                upper_address = u64::from(u16::from_be_bytes([payload[0], payload[1]])) << 4;
            }
            0x04 if payload.len() == 2 => {
                upper_address = u64::from(u16::from_be_bytes([payload[0], payload[1]])) << 16;
            }
            0x03 | 0x05 if payload.len() == 4 => {}
            record_type => {
                return Err(value_invalid(format!(
                    "Intel HEX 第 {} 行记录类型 0x{record_type:02X} 或长度无效",
                    index + 1
                )));
            }
        }
    }
    if !saw_eof {
        return Err(value_invalid("Intel HEX 缺少 EOF 记录"));
    }
    normalize_segments(raw_segments)
}

fn parse_s_record(data: &[u8]) -> Result<Vec<FirmwareSegment>, JlinkError> {
    let text =
        std::str::from_utf8(data).map_err(|_| value_invalid("S-record 必须是 UTF-8/ASCII 文本"))?;
    let mut raw_segments = Vec::new();
    let mut saw_termination = false;
    let mut saw_count = false;
    let mut data_record_count = 0_u64;
    for (index, untrimmed) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = untrimmed.trim();
        if line.is_empty() {
            continue;
        }
        if saw_termination {
            return Err(value_invalid(format!(
                "S-record 第 {} 行位于终止记录之后",
                index + 1
            )));
        }
        if !line.is_ascii() || line.len() < 4 || !line.as_bytes()[0].eq_ignore_ascii_case(&b'S') {
            return Err(value_invalid(format!("S-record 第 {} 行头无效", index + 1)));
        }
        let record_type = line.as_bytes()[1].to_ascii_uppercase();
        let address_bytes = match record_type {
            b'0' | b'1' | b'5' | b'9' => 2,
            b'2' | b'6' | b'8' => 3,
            b'3' | b'7' => 4,
            _ => {
                return Err(value_invalid(format!(
                    "S-record 第 {} 行类型无效",
                    index + 1
                )));
            }
        };
        let record = decode_hex(&line[2..], "S-record", index + 1)?;
        if record.is_empty() || record.len() != usize::from(record[0]) + 1 {
            return Err(value_invalid(format!(
                "S-record 第 {} 行记录长度无效",
                index + 1
            )));
        }
        if record.iter().copied().fold(0_u8, u8::wrapping_add) != u8::MAX {
            return Err(value_invalid(format!(
                "S-record 第 {} 行校验和无效",
                index + 1
            )));
        }
        if record.len() < address_bytes + 2 {
            return Err(value_invalid(format!(
                "S-record 第 {} 行地址长度无效",
                index + 1
            )));
        }
        let address = record[1..=address_bytes]
            .iter()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
        let payload = &record[address_bytes + 1..record.len() - 1];
        if saw_count && !matches!(record_type, b'7' | b'8' | b'9') {
            return Err(value_invalid(format!(
                "S-record 第 {} 行位于数据记录计数之后",
                index + 1
            )));
        }
        match record_type {
            b'1' | b'2' | b'3' => {
                if !payload.is_empty() {
                    raw_segments.push(segment(address, payload.to_vec())?);
                }
                data_record_count = data_record_count
                    .checked_add(1)
                    .ok_or_else(|| value_invalid("S-record 数据记录计数溢出"))?;
            }
            b'7' | b'8' | b'9' if payload.is_empty() => saw_termination = true,
            b'5' | b'6' if payload.is_empty() && !saw_count => {
                if address != data_record_count {
                    return Err(value_invalid(format!(
                        "S-record 第 {} 行声明 {address} 条数据记录，实际为 {data_record_count} 条",
                        index + 1
                    )));
                }
                saw_count = true;
            }
            b'0' => {}
            _ => {
                return Err(value_invalid(format!(
                    "S-record 第 {} 行控制记录长度无效",
                    index + 1
                )));
            }
        }
    }
    if !saw_termination {
        return Err(value_invalid("S-record 缺少终止记录"));
    }
    normalize_segments(raw_segments)
}

fn decode_hex(text: &str, format_name: &str, line: usize) -> Result<Vec<u8>, JlinkError> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(value_invalid(format!(
            "{format_name} 第 {line} 行十六进制长度无效"
        )));
    }
    (0..text.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&text[offset..offset + 2], 16)
                .map_err(|_| value_invalid(format!("{format_name} 第 {line} 行包含非十六进制字符")))
        })
        .collect()
}

fn segment(address: u64, data: Vec<u8>) -> Result<FirmwareSegment, JlinkError> {
    if data.is_empty() {
        return Err(value_invalid("固件段不能为空"));
    }
    let length = u64::try_from(data.len()).map_err(|_| value_invalid("固件段长度无法表示"))?;
    address
        .checked_add(length)
        .ok_or_else(|| value_invalid("固件段地址溢出"))?;
    Ok(FirmwareSegment { address, data })
}

fn normalize_segments(
    mut segments: Vec<FirmwareSegment>,
) -> Result<Vec<FirmwareSegment>, JlinkError> {
    if segments.is_empty() {
        return Err(value_invalid("固件镜像不包含可加载数据"));
    }
    segments.sort_by_key(|item| item.address);
    let mut normalized: Vec<FirmwareSegment> = Vec::with_capacity(segments.len());
    for current in segments {
        let Some(previous) = normalized.last_mut() else {
            normalized.push(current);
            continue;
        };
        let previous_end = previous
            .address
            .checked_add(
                u64::try_from(previous.data.len())
                    .map_err(|_| value_invalid("固件段长度无法表示"))?,
            )
            .ok_or_else(|| value_invalid("固件段地址溢出"))?;
        if current.address < previous_end {
            return Err(value_invalid("固件镜像包含重叠的加载段"));
        }
        if current.address == previous_end {
            previous.data.extend_from_slice(&current.data);
        } else {
            normalized.push(current);
        }
    }
    Ok(normalized)
}

fn identity_unknown(
    message: &'static str,
    expected: Option<&FirmwareSegmentFingerprint>,
) -> JlinkError {
    let mut error = JlinkError::new(ErrorCode::FirmwareIdentityUnknown, message, false);
    if let Some(expected) = expected {
        error = error
            .with_detail("address", json!(format!("0x{:X}", expected.address)))
            .with_detail("length", json!(expected.length));
    }
    error
}

fn value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}

fn sha256(data: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(data) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strong_plan() -> FirmwareIdentityPlan {
        let bytes = [b'J', b'L', b'I', b'D', 1, 5, b'b', b'u', b'i', b'l', b'd'];
        FirmwareIdentityPlan {
            elf_sha256: "ab".repeat(32),
            identity_block: Some(
                FirmwareIdentityBlock::parse(0x100, &bytes).expect("valid identity block"),
            ),
            legacy_segments: Vec::new(),
        }
    }

    #[test]
    fn fixed_identity_block_is_bounded_and_parses_build_id() {
        let mut bytes = vec![0_u8; MAX_FIRMWARE_IDENTITY_BYTES];
        bytes[..6].copy_from_slice(&[b'J', b'L', b'I', b'D', 1, 5]);
        bytes[6..11].copy_from_slice(b"build");
        let block = FirmwareIdentityBlock::parse(0, &bytes).expect("64-byte block is valid");
        assert_eq!(block.bytes().len(), MAX_FIRMWARE_IDENTITY_BYTES);
        assert_eq!(block.build_id(), "build");
        assert_eq!(block.format_version(), 1);
    }

    #[test]
    fn malformed_identity_symbol_is_rejected() {
        let error =
            FirmwareIdentityBlock::parse(0, b"BAD!\x01\x01x").expect_err("wrong magic must fail");
        assert_eq!(error.code, ErrorCode::ValueInvalid);
        let error = FirmwareIdentityBlock::parse(0, b"JLID\x02\x01x")
            .expect_err("unknown format must fail");
        assert_eq!(error.code, ErrorCode::ValueInvalid);
    }

    #[test]
    fn weak_identity_allows_validation_but_not_mutating_or_hss_use() {
        let plan = FirmwareIdentityPlan {
            elf_sha256: "ab".repeat(32),
            identity_block: None,
            legacy_segments: Vec::new(),
        };
        plan.validate()
            .expect("weak ELF identity is structurally valid");
        assert_eq!(plan.strength(), FirmwareIdentityStrength::Weak);
        let error = plan
            .ensure_strong()
            .expect_err("strong identity is required");
        assert_eq!(error.code, ErrorCode::FirmwareIdentityUnknown);
        assert_eq!(
            error.details.expect("identity details")["required_symbol"],
            json!(FIRMWARE_IDENTITY_SYMBOL)
        );
    }

    #[test]
    fn strong_identity_compares_only_the_fixed_block() {
        let plan = strong_plan();
        let expected = plan.identity_block().expect("strong block").bytes();
        plan.verify_target_bytes(Some(expected))
            .expect("exact bounded evidence matches");
        let mut mismatch = expected.to_vec();
        mismatch[6] ^= 1;
        assert_eq!(
            plan.verify_target_bytes(Some(&mismatch))
                .expect_err("changed build id fails")
                .code,
            ErrorCode::FirmwareElfMismatch
        );
        assert!(plan.segments().is_empty());
    }
}
