use serde::Serialize;

use crate::{ErrorCode, JlinkError};

/// Version of the normalized selector and immutable access-plan representation.
pub const ACCESS_PLAN_FORMAT_VERSION: u32 = 1;

/// An explicit element range applied after resolving a variable path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ElementSlice {
    start: u64,
    count: u64,
}

impl ElementSlice {
    /// Validates a non-empty range.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SliceRequired`] when `count` is zero or the range overflows.
    pub fn new(start: u64, count: u64) -> Result<Self, JlinkError> {
        if count == 0 {
            return Err(slice_required("slice.count 必须大于 0"));
        }
        start
            .checked_add(count)
            .ok_or_else(|| slice_required("slice 的元素范围溢出"))?;
        Ok(Self { start, count })
    }

    /// Returns the first selected element index.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the number of selected elements.
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }
}

/// One normalized path operation after the root variable name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SelectorStep {
    /// Selects one named structure or union member.
    Member(String),
    /// Selects one fixed-array element.
    Index(u64),
}

/// A validated variable path and its independent optional element slice.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VariableSelector {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slice: Option<ElementSlice>,
}

impl VariableSelector {
    /// Parses and normalizes a C variable/member/array path.
    ///
    /// Array indices are rendered in canonical decimal form. The independent
    /// `slice` remains separate and therefore cannot be replaced by a terminal
    /// path index.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] for an invalid path grammar.
    pub fn new(path: &str, slice: Option<ElementSlice>) -> Result<Self, JlinkError> {
        let (root, steps) = parse_selector_path(path)?;
        let mut normalized = root;
        for step in steps {
            match step {
                SelectorStep::Member(member) => {
                    normalized.push('.');
                    normalized.push_str(&member);
                }
                SelectorStep::Index(index) => {
                    normalized.push('[');
                    normalized.push_str(&index.to_string());
                    normalized.push(']');
                }
            }
        }
        Ok(Self {
            path: normalized,
            slice,
        })
    }

    /// Returns the normalized path without the independent slice.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the independent slice, when one was supplied.
    #[must_use]
    pub const fn slice(&self) -> Option<ElementSlice> {
        self.slice
    }

    /// Returns the exact root variable name.
    #[must_use]
    pub fn root(&self) -> &str {
        self.path.split(['.', '[']).next().unwrap_or_default()
    }

    /// Re-parses the normalized path into deterministic member and index steps.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] only if an internal invariant was violated.
    pub fn steps(&self) -> Result<Vec<SelectorStep>, JlinkError> {
        parse_selector_path(&self.path).map(|(_, steps)| steps)
    }

    /// Returns the normalized cache-key fragment including the independent slice.
    #[must_use]
    pub fn cache_fragment(&self) -> String {
        self.slice.map_or_else(
            || self.path.clone(),
            |slice| format!("{}|slice:{}:{}", self.path, slice.start(), slice.count()),
        )
    }
}

/// Scalar encoding retained from DWARF for lossless typed-value processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarEncoding {
    /// A two's-complement signed integer.
    Signed,
    /// An unsigned integer.
    Unsigned,
    /// A DWARF Boolean value.
    Boolean,
    /// An IEEE-754 floating-point value.
    Float,
    /// A scalar encoding outside the explicitly supported V1 set.
    Other,
}

/// A member of a structure or union layout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessMember {
    name: String,
    byte_offset: u64,
    storage_size: Option<u64>,
    dwarf_bit_offset: Option<u64>,
    bit_size: Option<u64>,
    layout: AccessLayout,
}

impl AccessMember {
    /// Creates one immutable aggregate member description.
    #[must_use]
    pub fn new(
        name: String,
        byte_offset: u64,
        storage_size: Option<u64>,
        dwarf_bit_offset: Option<u64>,
        bit_size: Option<u64>,
        layout: AccessLayout,
    ) -> Self {
        Self {
            name,
            byte_offset,
            storage_size,
            dwarf_bit_offset,
            bit_size,
            layout,
        }
    }

    /// Returns the exact DWARF member name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the byte offset from the aggregate base.
    #[must_use]
    pub const fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    /// Returns the selected member layout.
    #[must_use]
    pub const fn layout(&self) -> &AccessLayout {
        &self.layout
    }

    /// Returns the DWARF storage-unit size used by a bit-field member.
    #[must_use]
    pub const fn storage_size(&self) -> Option<u64> {
        self.storage_size
    }

    /// Returns the DWARF v4 most-significant-bit offset for a bit-field member.
    #[must_use]
    pub const fn dwarf_bit_offset(&self) -> Option<u64> {
        self.dwarf_bit_offset
    }

    /// Returns the logical bit width for a bit-field member.
    #[must_use]
    pub const fn bit_size(&self) -> Option<u64> {
        self.bit_size
    }
}

/// Recursive, address-independent DWARF value layout retained by an access plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessLayout {
    /// A base scalar value.
    Scalar {
        /// The DWARF type name, when present.
        name: String,
        /// Storage size in bytes.
        byte_size: u64,
        /// Scalar encoding used by typed-value conversion.
        encoding: ScalarEncoding,
    },
    /// A pointer represented only as an address value.
    Pointer {
        /// Pointer storage size in bytes.
        byte_size: u64,
    },
    /// A structure with deterministic member layouts.
    Structure {
        /// Structure storage size in bytes.
        byte_size: u64,
        /// Members in DWARF declaration order.
        members: Vec<AccessMember>,
    },
    /// A union whose active member is not inferred.
    Union {
        /// Union storage size in bytes.
        byte_size: u64,
        /// Members in DWARF declaration order.
        members: Vec<AccessMember>,
    },
    /// A fixed or explicitly bounded array.
    Array {
        /// Element layout.
        element: Box<Self>,
        /// Element count; `None` denotes an unbounded DWARF array before slicing.
        count: Option<u64>,
    },
}

impl AccessLayout {
    /// Returns the byte size when the layout is statically bounded.
    #[must_use]
    pub fn byte_size(&self) -> Option<u64> {
        match self {
            Self::Scalar { byte_size, .. } | Self::Pointer { byte_size } => Some(*byte_size),
            Self::Structure { byte_size, members } | Self::Union { byte_size, members } => members
                .iter()
                .all(|member| member.layout.byte_size().is_some())
                .then_some(*byte_size),
            Self::Array {
                element,
                count: Some(count),
            } => element.byte_size()?.checked_mul(*count),
            Self::Array { count: None, .. } => None,
        }
    }
}

/// A bit-field range within the plan's selected storage bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BitRange {
    lsb: u64,
    width: u64,
}

impl BitRange {
    /// Creates a bit range already validated against its storage unit.
    #[must_use]
    pub const fn new(lsb: u64, width: u64) -> Self {
        Self { lsb, width }
    }

    /// Returns the least-significant selected bit.
    #[must_use]
    pub const fn lsb(self) -> u64 {
        self.lsb
    }

    /// Returns the selected bit width.
    #[must_use]
    pub const fn width(self) -> u64 {
        self.width
    }
}

/// An immutable, target-independent plan for one exact DWARF selector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPlan {
    elf_sha256: String,
    parser_format_version: u32,
    selector: VariableSelector,
    address: u64,
    byte_size: u64,
    bit_range: Option<BitRange>,
    is_volatile: bool,
    layout: AccessLayout,
}

impl AccessPlan {
    /// Creates a plan from a resolved static address and selected layout.
    #[must_use]
    pub fn new(
        elf_sha256: String,
        selector: VariableSelector,
        address: u64,
        byte_size: u64,
        bit_range: Option<BitRange>,
        is_volatile: bool,
        layout: AccessLayout,
    ) -> Self {
        Self {
            elf_sha256,
            parser_format_version: ACCESS_PLAN_FORMAT_VERSION,
            selector,
            address,
            byte_size,
            bit_range,
            is_volatile,
            layout,
        }
    }

    /// Returns the lowercase SHA-256 identity of the complete symbol ELF.
    #[must_use]
    pub fn elf_sha256(&self) -> &str {
        &self.elf_sha256
    }

    /// Returns the parser format version included in the cache identity.
    #[must_use]
    pub const fn parser_format_version(&self) -> u32 {
        self.parser_format_version
    }

    /// Returns the normalized selector.
    #[must_use]
    pub const fn selector(&self) -> &VariableSelector {
        &self.selector
    }

    /// Returns the fixed target address.
    #[must_use]
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns the exact number of bytes required by the selected access.
    #[must_use]
    pub const fn byte_size(&self) -> u64 {
        self.byte_size
    }

    /// Returns optional bit-field extraction metadata.
    #[must_use]
    pub const fn bit_range(&self) -> Option<BitRange> {
        self.bit_range
    }

    /// Returns whether a volatile qualifier was encountered on the selected path.
    #[must_use]
    pub const fn is_volatile(&self) -> bool {
        self.is_volatile
    }

    /// Returns the selected recursive value layout.
    #[must_use]
    pub const fn layout(&self) -> &AccessLayout {
        &self.layout
    }
}

fn parse_selector_path(path: &str) -> Result<(String, Vec<SelectorStep>), JlinkError> {
    if path.is_empty() || path.trim() != path || !path.is_ascii() {
        return Err(value_invalid(
            "变量路径必须是非空、无首尾空白的 ASCII C 标识符路径",
        ));
    }
    let bytes = path.as_bytes();
    let (root, mut cursor) = parse_identifier(path, 0)?;
    let mut steps = Vec::new();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'.' => {
                let (member, next) = parse_identifier(path, cursor + 1)?;
                steps.push(SelectorStep::Member(member));
                cursor = next;
            }
            b'[' => {
                let start = cursor + 1;
                let Some(relative_end) = bytes[start..].iter().position(|byte| *byte == b']')
                else {
                    return Err(value_invalid("变量路径中的数组索引缺少右方括号"));
                };
                let end = start + relative_end;
                let digits = &path[start..end];
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(value_invalid("数组索引必须是非负十进制整数"));
                }
                let index = digits
                    .parse::<u64>()
                    .map_err(|_| value_invalid("数组索引超出 u64 范围"))?;
                steps.push(SelectorStep::Index(index));
                cursor = end + 1;
            }
            _ => return Err(value_invalid("变量路径只允许成员点号和数组索引")),
        }
    }
    Ok((root, steps))
}

fn parse_identifier(path: &str, start: usize) -> Result<(String, usize), JlinkError> {
    let bytes = path.as_bytes();
    let Some(first) = bytes.get(start).copied() else {
        return Err(value_invalid("变量路径缺少标识符"));
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Err(value_invalid("变量标识符必须以 ASCII 字母或下划线开头"));
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        end += 1;
    }
    Ok((path[start..end].to_owned(), end))
}

fn value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}

fn slice_required(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::SliceRequired, message, false)
}
