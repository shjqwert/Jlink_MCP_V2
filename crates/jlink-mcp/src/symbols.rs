use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::Path,
    sync::Arc,
};

use gimli::{
    AttributeValue, Dwarf, DwarfSections, EndianSlice, EntriesTreeNode, Operation, Reader,
    RunTimeEndian, Unit, UnitType,
};
use jlink_domain::{
    ACCESS_PLAN_FORMAT_VERSION, AccessLayout, AccessMember, AccessPlan, BitRange, ElementSlice,
    ErrorCode, JlinkError, ScalarEncoding, SelectorStep, VariableSelector,
};
use object::{Architecture, BinaryFormat, Object, ObjectSection};
use sha2::{Digest, Sha256};

type DwarfReader<'data> = EndianSlice<'data, RunTimeEndian>;
type TypeId = u64;

/// A parsed, immutable index of exact DWARF variable paths from one ELF identity.
#[derive(Debug)]
pub struct SymbolIndex {
    elf_sha256: String,
    dwarf_versions: BTreeSet<u16>,
    producers: BTreeSet<String>,
    index: DwarfIndex,
    direct_paths: Vec<String>,
}

impl SymbolIndex {
    /// Parses an ARM little-endian ELF and indexes its DWARF information.
    ///
    /// # Errors
    ///
    /// Returns a stable value or type error when the artifact is not a supported
    /// ELF/DWARF input or contains an unsupported reference form.
    pub fn from_elf_bytes(data: &[u8]) -> Result<Self, JlinkError> {
        if data.is_empty() {
            return Err(value_invalid("符号 ELF 不能为空"));
        }
        let object = object::File::parse(data)
            .map_err(|error| value_invalid(format!("无法解析符号 ELF：{error}")))?;
        if object.format() != BinaryFormat::Elf {
            return Err(value_invalid("符号输入必须是 ELF/AXF/OUT 文件"));
        }
        if object.architecture() != Architecture::Arm || !object.is_little_endian() {
            return Err(type_unsupported(
                "V1 DWARF 解析只支持 ARM little-endian ELF",
            ));
        }
        let elf_sha256 = sha256(data);
        let endian = RunTimeEndian::Little;
        let sections = DwarfSections::load(|section_id| -> Result<Cow<'_, [u8]>, object::Error> {
            object.section_by_name(section_id.name()).map_or_else(
                || Ok(Cow::Borrowed(&[] as &[u8])),
                |section| section.uncompressed_data(),
            )
        })
        .map_err(|error| value_invalid(format!("无法加载 DWARF section：{error}")))?;
        let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
        let mut index = index_dwarf(&dwarf)?;
        validate_dwarf_versions(&index.versions)?;
        let dwarf_versions = std::mem::take(&mut index.versions);
        let producers = std::mem::take(&mut index.producers);
        let direct_paths = collect_direct_paths(&index);
        Ok(Self {
            elf_sha256,
            dwarf_versions,
            producers,
            index,
            direct_paths,
        })
    }

    /// Reads and parses one ELF path.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when the file cannot be read, plus the
    /// parsing errors documented by [`Self::from_elf_bytes`].
    pub fn from_elf_path(path: &Path) -> Result<Self, JlinkError> {
        let data = fs::read(path).map_err(|error| {
            value_invalid(format!("无法读取符号 ELF {}：{error}", path.display()))
        })?;
        Self::from_elf_bytes(&data)
    }

    /// Returns the lowercase SHA-256 identity of the complete ELF bytes.
    #[must_use]
    pub fn elf_sha256(&self) -> &str {
        &self.elf_sha256
    }

    /// Returns all observed DWARF versions in ascending order.
    #[must_use]
    pub fn dwarf_versions(&self) -> Vec<u16> {
        self.dwarf_versions.iter().copied().collect()
    }

    /// Returns all observed producer strings in stable ascending order.
    #[must_use]
    pub fn producers(&self) -> Vec<&str> {
        self.producers.iter().map(String::as_str).collect()
    }

    /// Returns the number of parsed `.debug_info` compile units.
    #[must_use]
    pub const fn unit_count(&self) -> u64 {
        self.index.unit_count
    }

    /// Returns the number of parsed `.debug_types` type units.
    #[must_use]
    pub const fn type_unit_count(&self) -> u64 {
        self.index.type_unit_count
    }

    /// Returns the number of indexed type definitions.
    #[must_use]
    pub fn type_count(&self) -> usize {
        self.index.types.len()
    }

    /// Returns the number of resolved `DW_FORM_ref_sig8` type references.
    #[must_use]
    pub const fn signature_reference_count(&self) -> u64 {
        self.index.signature_reference_count
    }

    /// Returns the number of non-declaration variable definitions.
    #[must_use]
    pub fn variable_definition_count(&self) -> usize {
        self.index
            .variables
            .values()
            .flatten()
            .filter(|variable| !variable.declaration)
            .count()
    }

    /// Returns the number of stable paths currently eligible for symbol search.
    #[must_use]
    pub fn direct_path_count(&self) -> usize {
        self.direct_paths.len()
    }

    /// Searches exact, directly usable variable paths in stable order.
    ///
    /// Matching is an ASCII case-insensitive substring search. Returned paths
    /// preserve the exact DWARF spelling.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when `query` is blank or `limit` is
    /// outside the public range `1..=50`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<String>, JlinkError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(value_invalid("symbols.query 不能为空"));
        }
        if !(1..=50).contains(&limit) {
            return Err(value_invalid("symbols.limit 必须在 1 到 50 之间"));
        }
        let needle = query.to_ascii_lowercase();
        Ok(self
            .direct_paths
            .iter()
            .filter(|path| path.to_ascii_lowercase().contains(&needle))
            .take(limit)
            .cloned()
            .collect())
    }

    /// Resolves one exact selector into a fixed immutable access plan.
    ///
    /// # Errors
    ///
    /// Returns `SYMBOL_NOT_FOUND`, `SYMBOL_AMBIGUOUS`,
    /// `DYNAMIC_LOCATION_UNSUPPORTED`, or `TYPE_UNSUPPORTED` when the selector
    /// cannot form one fixed V1 access plan.
    pub fn access_plan(&self, selector: &VariableSelector) -> Result<AccessPlan, JlinkError> {
        resolve_access_plan(&self.index, &self.elf_sha256, selector)
    }
}

/// Observable cache sizes used to prove cache-key and invalidation behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolCacheStats {
    /// Number of distinct ELF content identities indexed.
    pub elf_indexes: usize,
    /// Number of immutable access plans cached.
    pub access_plans: usize,
}

/// Process-owned cache keyed only by ELF content and normalized selector identity.
#[derive(Default)]
pub struct SymbolCache {
    indexes: BTreeMap<String, Arc<SymbolIndex>>,
    plans: BTreeMap<AccessPlanCacheKey, AccessPlan>,
}

impl SymbolCache {
    /// Creates an empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            indexes: BTreeMap::new(),
            plans: BTreeMap::new(),
        }
    }

    /// Loads or reuses an index for the exact ELF file contents.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`SymbolIndex::from_elf_path`].
    pub fn load_path(&mut self, path: &Path) -> Result<Arc<SymbolIndex>, JlinkError> {
        let data = fs::read(path).map_err(|error| {
            value_invalid(format!("无法读取符号 ELF {}：{error}", path.display()))
        })?;
        self.load_bytes(&data)
    }

    /// Loads or reuses an index for the exact ELF bytes.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`SymbolIndex::from_elf_bytes`].
    pub fn load_bytes(&mut self, data: &[u8]) -> Result<Arc<SymbolIndex>, JlinkError> {
        let identity = sha256(data);
        if let Some(index) = self.indexes.get(&identity) {
            return Ok(Arc::clone(index));
        }
        let index = Arc::new(SymbolIndex::from_elf_bytes(data)?);
        self.indexes.insert(identity, Arc::clone(&index));
        Ok(index)
    }

    /// Resolves or reuses a plan under the exact three-part cache key.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`SymbolIndex::access_plan`].
    pub fn access_plan(
        &mut self,
        index: &SymbolIndex,
        selector: &VariableSelector,
    ) -> Result<AccessPlan, JlinkError> {
        let key = AccessPlanCacheKey {
            elf_sha256: index.elf_sha256().to_owned(),
            normalized_selector: selector.cache_fragment(),
            parser_format_version: ACCESS_PLAN_FORMAT_VERSION,
        };
        if let Some(plan) = self.plans.get(&key) {
            return Ok(plan.clone());
        }
        let plan = index.access_plan(selector)?;
        self.plans.insert(key, plan.clone());
        Ok(plan)
    }

    /// Returns current cache cardinalities without exposing mutable cache entries.
    #[must_use]
    pub fn stats(&self) -> SymbolCacheStats {
        SymbolCacheStats {
            elf_indexes: self.indexes.len(),
            access_plans: self.plans.len(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AccessPlanCacheKey {
    elf_sha256: String,
    normalized_selector: String,
    parser_format_version: u32,
}

#[derive(Clone, Debug)]
enum TypeNode {
    Base {
        name: String,
        byte_size: u64,
        encoding: ScalarEncoding,
    },
    Typedef {
        target: TypeId,
    },
    Qualifier {
        target: TypeId,
        is_volatile: bool,
    },
    Pointer {
        byte_size: u64,
    },
    Structure {
        byte_size: u64,
        members: Vec<Member>,
    },
    Union {
        byte_size: u64,
        members: Vec<Member>,
    },
    Array {
        element: TypeId,
        count: Option<u64>,
    },
}

#[derive(Clone, Debug)]
struct Member {
    name: String,
    type_id: TypeId,
    byte_offset: Option<u64>,
    storage_size: Option<u64>,
    bit_size: Option<u64>,
    dwarf_bit_offset: Option<u64>,
    data_bit_offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NormalizedMember {
    byte_offset: u64,
    storage_size: Option<u64>,
    dwarf_bit_offset: Option<u64>,
    bit_size: Option<u64>,
    bit_range: Option<BitRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VariableLocation {
    Static(u64),
    Dynamic,
}

#[derive(Clone, Debug)]
struct Variable {
    type_id: TypeId,
    location: VariableLocation,
    declaration: bool,
}

#[derive(Default, Debug)]
struct DwarfIndex {
    types: BTreeMap<TypeId, TypeNode>,
    variables: BTreeMap<String, Vec<Variable>>,
    producers: BTreeSet<String>,
    versions: BTreeSet<u16>,
    type_signatures: BTreeMap<u64, TypeId>,
    signature_reference_count: u64,
    unit_count: u64,
    type_unit_count: u64,
}

fn index_dwarf(dwarf: &Dwarf<DwarfReader<'_>>) -> Result<DwarfIndex, JlinkError> {
    let mut index = DwarfIndex::default();
    let mut type_units = dwarf.type_units();
    while let Some(header) = type_units
        .next()
        .map_err(|error| dwarf_error("读取 type unit header", error))?
    {
        if let UnitType::Type {
            type_signature,
            type_offset,
        } = header.type_()
        {
            let offset = type_offset
                .to_debug_types_offset(&header)
                .ok_or_else(|| type_unsupported("type unit 定义不在 .debug_types section 中"))?;
            index
                .type_signatures
                .insert(type_signature.0, type_id_from_types_offset(offset.0));
        }
    }

    let mut units = dwarf.units();
    while let Some(header) = units
        .next()
        .map_err(|error| dwarf_error("读取 compile unit header", error))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|error| dwarf_error("读取 compile unit", error))?;
        index.unit_count += 1;
        index.versions.insert(unit.header.version());
        let mut tree = unit
            .entries_tree(None)
            .map_err(|error| dwarf_error("打开 compile unit DIE tree", error))?;
        let root = tree
            .root()
            .map_err(|error| dwarf_error("读取 compile unit 根 DIE", error))?;
        process_node(dwarf, &unit, root, None, &mut index)?;
    }

    let mut type_units = dwarf.type_units();
    while let Some(header) = type_units
        .next()
        .map_err(|error| dwarf_error("再次读取 type unit header", error))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|error| dwarf_error("读取 type unit", error))?;
        index.type_unit_count += 1;
        index.versions.insert(unit.header.version());
        let mut tree = unit
            .entries_tree(None)
            .map_err(|error| dwarf_error("打开 type unit DIE tree", error))?;
        let root = tree
            .root()
            .map_err(|error| dwarf_error("读取 type unit 根 DIE", error))?;
        process_node(dwarf, &unit, root, None, &mut index)?;
    }
    Ok(index)
}

fn process_node<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    node: EntriesTreeNode<'_, '_, '_, R>,
    owner: Option<TypeId>,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    let child_owner = process_entry(dwarf, unit, node.entry(), owner, index)?;
    let mut children = node.children();
    while let Some(child) = children
        .next()
        .map_err(|error| dwarf_error("读取子 DIE", error))?
    {
        process_node(dwarf, unit, child, child_owner, index)?;
    }
    Ok(())
}

fn process_entry<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    owner: Option<TypeId>,
    index: &mut DwarfIndex,
) -> Result<Option<TypeId>, JlinkError> {
    let tag = entry.tag();
    let id = entry_type_id(entry, unit)?;
    let name = attr_string(
        dwarf,
        unit,
        attribute(entry, gimli::DW_AT_name, "DW_AT_name")?,
    )?;
    let byte_size =
        attr_udata(attribute(entry, gimli::DW_AT_byte_size, "DW_AT_byte_size")?).unwrap_or(0);
    let referenced_type = type_reference(
        attribute(entry, gimli::DW_AT_type, "DW_AT_type")?,
        unit,
        index,
    )?;
    match tag {
        gimli::DW_TAG_compile_unit => record_compile_unit(dwarf, unit, entry, index)?,
        gimli::DW_TAG_base_type => record_base_type(entry, id, name, byte_size, index)?,
        gimli::DW_TAG_typedef => {
            if let Some(target) = referenced_type {
                index.types.insert(id, TypeNode::Typedef { target });
            }
        }
        gimli::DW_TAG_const_type | gimli::DW_TAG_restrict_type | gimli::DW_TAG_volatile_type => {
            if let Some(target) = referenced_type {
                index.types.insert(
                    id,
                    TypeNode::Qualifier {
                        target,
                        is_volatile: tag == gimli::DW_TAG_volatile_type,
                    },
                );
            }
        }
        gimli::DW_TAG_pointer_type => record_pointer_type(unit, id, byte_size, index),
        gimli::DW_TAG_structure_type => {
            record_aggregate_type(id, byte_size, false, index);
            return Ok(Some(id));
        }
        gimli::DW_TAG_union_type => {
            record_aggregate_type(id, byte_size, true, index);
            return Ok(Some(id));
        }
        gimli::DW_TAG_array_type => {
            if let Some(element) = referenced_type {
                index.types.insert(
                    id,
                    TypeNode::Array {
                        element,
                        count: None,
                    },
                );
                return Ok(Some(id));
            }
        }
        gimli::DW_TAG_subrange_type => record_subrange(entry, owner, index)?,
        gimli::DW_TAG_member => record_member(entry, unit, owner, name, referenced_type, index)?,
        gimli::DW_TAG_variable => {
            record_variable(entry, unit, name, referenced_type, index)?;
        }
        _ => {}
    }
    Ok(None)
}

fn record_compile_unit<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    if let Some(producer) = attr_string(
        dwarf,
        unit,
        attribute(entry, gimli::DW_AT_producer, "DW_AT_producer")?,
    )? {
        index.producers.insert(producer);
    }
    Ok(())
}

fn record_base_type<R: Reader>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    id: TypeId,
    name: Option<String>,
    byte_size: u64,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    let encoding_value = attribute(entry, gimli::DW_AT_encoding, "DW_AT_encoding")?;
    index.types.insert(
        id,
        TypeNode::Base {
            name: name.unwrap_or_else(|| "<anonymous-base>".to_owned()),
            byte_size,
            encoding: scalar_encoding(encoding_value.as_ref()),
        },
    );
    Ok(())
}

fn record_pointer_type<R: Reader>(
    unit: &Unit<R>,
    id: TypeId,
    byte_size: u64,
    index: &mut DwarfIndex,
) {
    index.types.insert(
        id,
        TypeNode::Pointer {
            byte_size: if byte_size == 0 {
                u64::from(unit.header.address_size())
            } else {
                byte_size
            },
        },
    );
}

fn record_aggregate_type(id: TypeId, byte_size: u64, is_union: bool, index: &mut DwarfIndex) {
    let node = if is_union {
        TypeNode::Union {
            byte_size,
            members: Vec::new(),
        }
    } else {
        TypeNode::Structure {
            byte_size,
            members: Vec::new(),
        }
    };
    index.types.insert(id, node);
}

fn record_subrange<R: Reader>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    owner: Option<TypeId>,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    let Some(TypeNode::Array { count, .. }) =
        owner.and_then(|owner_id| index.types.get_mut(&owner_id))
    else {
        return Ok(());
    };
    let explicit_count = attr_udata(attribute(entry, gimli::DW_AT_count, "DW_AT_count")?);
    let upper_bound = attr_udata(attribute(
        entry,
        gimli::DW_AT_upper_bound,
        "DW_AT_upper_bound",
    )?);
    *count = explicit_count.or_else(|| upper_bound.and_then(normalize_upper_bound));
    Ok(())
}

fn record_member<R: Reader>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    unit: &Unit<R>,
    owner: Option<TypeId>,
    name: Option<String>,
    referenced_type: Option<TypeId>,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    let (Some(owner_id), Some(name), Some(type_id)) = (owner, name, referenced_type) else {
        return Ok(());
    };
    let member = Member {
        name,
        type_id,
        byte_offset: member_offset(
            attribute(
                entry,
                gimli::DW_AT_data_member_location,
                "DW_AT_data_member_location",
            )?,
            unit,
        )?,
        storage_size: attr_udata(attribute(entry, gimli::DW_AT_byte_size, "成员 byte_size")?),
        bit_size: attr_udata(attribute(entry, gimli::DW_AT_bit_size, "DW_AT_bit_size")?),
        dwarf_bit_offset: attr_udata(attribute(
            entry,
            gimli::DW_AT_bit_offset,
            "DW_AT_bit_offset",
        )?),
        data_bit_offset: attr_udata(attribute(
            entry,
            gimli::DW_AT_data_bit_offset,
            "DW_AT_data_bit_offset",
        )?),
    };
    match index.types.get_mut(&owner_id) {
        Some(TypeNode::Structure { members, .. } | TypeNode::Union { members, .. }) => {
            members.push(member);
            Ok(())
        }
        _ => Err(type_unsupported(format!(
            "DWARF member owner {owner_id:#x} 不是 aggregate"
        ))),
    }
}

fn record_variable<R: Reader>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    unit: &Unit<R>,
    name: Option<String>,
    referenced_type: Option<TypeId>,
    index: &mut DwarfIndex,
) -> Result<(), JlinkError> {
    let (Some(name), Some(type_id)) = (name, referenced_type) else {
        return Ok(());
    };
    let location = variable_location(
        attribute(entry, gimli::DW_AT_location, "DW_AT_location")?,
        unit,
    )?;
    let declaration = matches!(
        attribute(entry, gimli::DW_AT_declaration, "DW_AT_declaration")?,
        Some(AttributeValue::Flag(true))
    );
    index.variables.entry(name).or_default().push(Variable {
        type_id,
        location,
        declaration,
    });
    Ok(())
}

fn attribute<R: Reader>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    name: gimli::DwAt,
    context: &str,
) -> Result<Option<AttributeValue<R>>, JlinkError> {
    entry
        .attr_value(name)
        .map_err(|error| dwarf_error(&format!("读取 {context}"), error))
}

fn entry_type_id<R: Reader<Offset = usize>>(
    entry: &gimli::DebuggingInformationEntry<'_, '_, R>,
    unit: &Unit<R>,
) -> Result<TypeId, JlinkError> {
    if let Some(offset) = entry.offset().to_debug_info_offset(&unit.header) {
        return Ok(offset.0 as u64);
    }
    entry
        .offset()
        .to_debug_types_offset(&unit.header)
        .map(|offset| type_id_from_types_offset(offset.0))
        .ok_or_else(|| type_unsupported("DIE 不在 .debug_info 或 .debug_types 中"))
}

const fn type_id_from_types_offset(offset: usize) -> TypeId {
    offset as u64 | (1_u64 << 63)
}

fn attr_string<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    value: Option<AttributeValue<R>>,
) -> Result<Option<String>, JlinkError> {
    value
        .map(|value| {
            let reader = dwarf
                .attr_string(unit, value)
                .map_err(|error| dwarf_error("解析 DWARF 字符串引用", error))?;
            reader
                .to_string_lossy()
                .map(Cow::into_owned)
                .map_err(|error| dwarf_error("解码 DWARF 字符串", error))
        })
        .transpose()
}

fn attr_udata<R: Reader>(value: Option<AttributeValue<R>>) -> Option<u64> {
    value.and_then(|value| value.udata_value())
}

fn scalar_encoding<R: Reader>(value: Option<&AttributeValue<R>>) -> ScalarEncoding {
    match value {
        Some(AttributeValue::Encoding(value))
            if *value == gimli::DW_ATE_signed || *value == gimli::DW_ATE_signed_char =>
        {
            ScalarEncoding::Signed
        }
        Some(AttributeValue::Encoding(value))
            if *value == gimli::DW_ATE_unsigned || *value == gimli::DW_ATE_unsigned_char =>
        {
            ScalarEncoding::Unsigned
        }
        Some(AttributeValue::Encoding(value)) if *value == gimli::DW_ATE_boolean => {
            ScalarEncoding::Boolean
        }
        Some(AttributeValue::Encoding(value)) if *value == gimli::DW_ATE_float => {
            ScalarEncoding::Float
        }
        _ => ScalarEncoding::Other,
    }
}

fn type_reference<R: Reader<Offset = usize>>(
    value: Option<AttributeValue<R>>,
    unit: &Unit<R>,
    index: &mut DwarfIndex,
) -> Result<Option<TypeId>, JlinkError> {
    match value {
        Some(AttributeValue::UnitRef(offset)) => {
            if let Some(offset) = offset.to_debug_info_offset(&unit.header) {
                Ok(Some(offset.0 as u64))
            } else {
                offset
                    .to_debug_types_offset(&unit.header)
                    .map(|offset| Some(type_id_from_types_offset(offset.0)))
                    .ok_or_else(|| type_unsupported("unit-relative type reference 缺少 section"))
            }
        }
        Some(AttributeValue::DebugInfoRef(offset)) => Ok(Some(offset.0 as u64)),
        Some(AttributeValue::DebugTypesRef(signature)) => {
            let type_id = index
                .type_signatures
                .get(&signature.0)
                .copied()
                .ok_or_else(|| {
                    type_unsupported(format!(
                        "DW_FORM_ref_sig8 引用的 type signature {:#x} 不存在",
                        signature.0
                    ))
                })?;
            index.signature_reference_count += 1;
            Ok(Some(type_id))
        }
        None => Ok(None),
        Some(other) => Err(type_unsupported(format!(
            "不支持的 DWARF type reference：{other:?}"
        ))),
    }
}

fn normalize_upper_bound(value: u64) -> Option<u64> {
    if value == u64::from(u32::MAX) {
        None
    } else {
        value.checked_add(1)
    }
}

fn member_offset<R: Reader>(
    value: Option<AttributeValue<R>>,
    unit: &Unit<R>,
) -> Result<Option<u64>, JlinkError> {
    match value {
        Some(value) if value.udata_value().is_some() => Ok(value.udata_value()),
        Some(AttributeValue::Exprloc(expression)) => {
            let mut operations = expression.operations(unit.encoding());
            let first = operations
                .next()
                .map_err(|error| dwarf_error("解码 member location", error))?;
            match first {
                Some(Operation::PlusConstant { value })
                    if operations
                        .next()
                        .map_err(|error| dwarf_error("检查 member location 尾部", error))?
                        .is_none() =>
                {
                    Ok(Some(value))
                }
                _ => Ok(None),
            }
        }
        None => Ok(Some(0)),
        Some(_) => Ok(None),
    }
}

fn variable_location<R: Reader>(
    value: Option<AttributeValue<R>>,
    unit: &Unit<R>,
) -> Result<VariableLocation, JlinkError> {
    match value {
        Some(AttributeValue::Exprloc(expression)) => {
            let mut operations = expression.operations(unit.encoding());
            let first = operations
                .next()
                .map_err(|error| dwarf_error("解码 variable location", error))?;
            match first {
                Some(Operation::Address { address })
                    if operations
                        .next()
                        .map_err(|error| dwarf_error("检查 variable location 尾部", error))?
                        .is_none() =>
                {
                    Ok(VariableLocation::Static(address))
                }
                _ => Ok(VariableLocation::Dynamic),
            }
        }
        Some(AttributeValue::Addr(address)) => Ok(VariableLocation::Static(address)),
        _ => Ok(VariableLocation::Dynamic),
    }
}

fn resolve_access_plan(
    index: &DwarfIndex,
    elf_sha256: &str,
    selector: &VariableSelector,
) -> Result<AccessPlan, JlinkError> {
    let mut resolved = resolve_root(index, selector)?;
    for step in selector.steps()? {
        apply_selector_step(index, selector, &mut resolved, step)?;
    }
    let unwrapped = unwrap_type(index, resolved.type_id)?;
    resolved.type_id = unwrapped.type_id;
    resolved.is_volatile |= unwrapped.is_volatile;
    let (layout, byte_size) = if let Some(slice) = selector.slice() {
        let slice = resolve_slice(index, resolved.type_id, resolved.address, slice)?;
        resolved.address = slice.address;
        (slice.layout, slice.byte_size)
    } else {
        let layout = layout_from_type(index, resolved.type_id, &mut BTreeSet::new())?;
        let Some(byte_size) = resolved
            .selected_storage_size
            .or_else(|| layout.byte_size())
        else {
            if matches!(layout, AccessLayout::Array { count: None, .. }) {
                return Err(slice_required("柔性数组需要独立显式 slice"));
            }
            return Err(type_unsupported(
                "选中 aggregate 包含无界成员；请直接选择柔性数组成员并提供 slice",
            ));
        };
        (layout, byte_size)
    };
    Ok(AccessPlan::new(
        elf_sha256.to_owned(),
        selector.clone(),
        resolved.address,
        byte_size,
        resolved.bit_range,
        resolved.is_volatile,
        layout,
    ))
}

struct ResolutionState {
    address: u64,
    type_id: TypeId,
    is_volatile: bool,
    selected_storage_size: Option<u64>,
    bit_range: Option<BitRange>,
}

fn resolve_root(
    index: &DwarfIndex,
    selector: &VariableSelector,
) -> Result<ResolutionState, JlinkError> {
    let candidates = index
        .variables
        .get(selector.root())
        .ok_or_else(|| symbol_not_found(selector.path()))?;
    let definitions: Vec<&Variable> = candidates
        .iter()
        .filter(|variable| !variable.declaration)
        .collect();
    let variable = match definitions.as_slice() {
        [] => return Err(symbol_not_found(selector.path())),
        [variable] => *variable,
        _ => return Err(symbol_ambiguous(selector.root(), definitions.len())),
    };
    let address = match variable.location {
        VariableLocation::Static(address) => address,
        VariableLocation::Dynamic => return Err(dynamic_location(selector.root())),
    };
    Ok(ResolutionState {
        address,
        type_id: variable.type_id,
        is_volatile: false,
        selected_storage_size: None,
        bit_range: None,
    })
}

fn apply_selector_step(
    index: &DwarfIndex,
    selector: &VariableSelector,
    resolved: &mut ResolutionState,
    step: SelectorStep,
) -> Result<(), JlinkError> {
    let unwrapped = unwrap_type(index, resolved.type_id)?;
    resolved.type_id = unwrapped.type_id;
    resolved.is_volatile |= unwrapped.is_volatile;
    match step {
        SelectorStep::Member(name) => apply_member_step(index, selector, resolved, &name),
        SelectorStep::Index(element_index) => apply_index_step(index, resolved, element_index),
    }
}

fn apply_member_step(
    index: &DwarfIndex,
    selector: &VariableSelector,
    resolved: &mut ResolutionState,
    name: &str,
) -> Result<(), JlinkError> {
    let members = match index.types.get(&resolved.type_id) {
        Some(TypeNode::Structure { members, .. } | TypeNode::Union { members, .. }) => members,
        Some(TypeNode::Pointer { .. }) => {
            return Err(type_unsupported("V1 不自动跟随指针；只能读取指针地址本身"));
        }
        _ => {
            return Err(type_unsupported(format!(
                "路径成员 .{name} 只能应用于结构体或 union"
            )));
        }
    };
    let mut matches = members.iter().filter(|member| member.name == name);
    let member = matches
        .next()
        .ok_or_else(|| symbol_not_found(selector.path()))?;
    if matches.next().is_some() {
        return Err(symbol_ambiguous(selector.path(), 2));
    }
    let member_type_id = member.type_id;
    let member = normalize_member(index, member)?;
    resolved.address = resolved
        .address
        .checked_add(member.byte_offset)
        .ok_or_else(|| type_unsupported("成员地址计算溢出"))?;
    resolved.type_id = member_type_id;
    resolved.selected_storage_size = member.storage_size;
    resolved.bit_range = member.bit_range;
    Ok(())
}

fn apply_index_step(
    index: &DwarfIndex,
    resolved: &mut ResolutionState,
    element_index: u64,
) -> Result<(), JlinkError> {
    let (element, count) = match index.types.get(&resolved.type_id) {
        Some(TypeNode::Array { element, count }) => (*element, *count),
        _ => return Err(type_unsupported("数组索引只能应用于数组")),
    };
    let Some(count) = count else {
        return Err(slice_required("柔性数组不能用路径 [i] 替代独立 slice"));
    };
    if element_index >= count {
        return Err(value_invalid(format!(
            "数组索引 {element_index} 超出固定长度 {count}"
        )));
    }
    let element_size = type_size(index, element)?;
    let offset = element_index
        .checked_mul(element_size)
        .ok_or_else(|| type_unsupported("数组索引 offset 溢出"))?;
    resolved.address = resolved
        .address
        .checked_add(offset)
        .ok_or_else(|| type_unsupported("数组元素地址溢出"))?;
    resolved.type_id = element;
    Ok(())
}

struct ResolvedSlice {
    address: u64,
    byte_size: u64,
    layout: AccessLayout,
}

fn resolve_slice(
    index: &DwarfIndex,
    type_id: TypeId,
    address: u64,
    slice: ElementSlice,
) -> Result<ResolvedSlice, JlinkError> {
    let (element, bound) = match index.types.get(&type_id) {
        Some(TypeNode::Array { element, count }) => (*element, *count),
        _ => return Err(value_invalid("slice 只能应用于数组选择结果")),
    };
    let end = slice
        .start()
        .checked_add(slice.count())
        .ok_or_else(|| value_invalid("slice 范围溢出"))?;
    if bound.is_some_and(|bound| end > bound) {
        return Err(value_invalid("slice 超出固定数组边界"));
    }
    let element_layout = layout_from_type(index, element, &mut BTreeSet::new())?;
    let element_size = element_layout
        .byte_size()
        .ok_or_else(|| type_unsupported("slice 元素类型没有固定大小"))?;
    let byte_offset = slice
        .start()
        .checked_mul(element_size)
        .ok_or_else(|| slice_range_error(bound, "slice offset 溢出"))?;
    let byte_size = slice
        .count()
        .checked_mul(element_size)
        .ok_or_else(|| slice_range_error(bound, "slice byte size 溢出"))?;
    Ok(ResolvedSlice {
        address: address
            .checked_add(byte_offset)
            .ok_or_else(|| slice_range_error(bound, "slice 地址溢出"))?,
        byte_size,
        layout: AccessLayout::Array {
            element: Box::new(element_layout),
            count: Some(slice.count()),
        },
    })
}

#[derive(Clone, Copy)]
struct UnwrappedType {
    type_id: TypeId,
    is_volatile: bool,
}

fn unwrap_type(index: &DwarfIndex, mut type_id: TypeId) -> Result<UnwrappedType, JlinkError> {
    let mut visited = BTreeSet::new();
    let mut is_volatile = false;
    loop {
        if !visited.insert(type_id) {
            return Err(type_unsupported(format!(
                "DWARF type reference 在 {type_id:#x} 形成循环"
            )));
        }
        match index.types.get(&type_id) {
            Some(TypeNode::Typedef { target }) => type_id = *target,
            Some(TypeNode::Qualifier {
                target,
                is_volatile: qualifier_is_volatile,
            }) => {
                is_volatile |= *qualifier_is_volatile;
                type_id = *target;
            }
            Some(_) => {
                return Ok(UnwrappedType {
                    type_id,
                    is_volatile,
                });
            }
            None => {
                return Err(type_unsupported(format!("DWARF type {type_id:#x} 不存在")));
            }
        }
    }
}

fn type_size(index: &DwarfIndex, type_id: TypeId) -> Result<u64, JlinkError> {
    let type_id = unwrap_type(index, type_id)?.type_id;
    match index.types.get(&type_id) {
        Some(
            TypeNode::Base { byte_size, .. }
            | TypeNode::Pointer { byte_size }
            | TypeNode::Structure { byte_size, .. }
            | TypeNode::Union { byte_size, .. },
        ) => Ok(*byte_size),
        Some(TypeNode::Array {
            element,
            count: Some(count),
        }) => count
            .checked_mul(type_size(index, *element)?)
            .ok_or_else(|| type_unsupported("数组 byte size 溢出")),
        Some(TypeNode::Array { count: None, .. }) => {
            Err(slice_required("柔性数组需要独立显式 slice"))
        }
        Some(TypeNode::Typedef { .. } | TypeNode::Qualifier { .. }) => unreachable!(),
        None => Err(type_unsupported(format!("DWARF type {type_id:#x} 不存在"))),
    }
}

fn normalize_member(index: &DwarfIndex, member: &Member) -> Result<NormalizedMember, JlinkError> {
    if member.data_bit_offset.is_some() && member.dwarf_bit_offset.is_some() {
        return Err(type_unsupported(format!(
            "成员 {} 同时包含 DW_AT_data_bit_offset 和 DW_AT_bit_offset",
            member.name
        )));
    }

    match (
        member.data_bit_offset,
        member.dwarf_bit_offset,
        member.bit_size,
    ) {
        (None, None, None) => Ok(NormalizedMember {
            byte_offset: member
                .byte_offset
                .ok_or_else(|| dynamic_location(&member.name))?,
            storage_size: None,
            dwarf_bit_offset: None,
            bit_size: None,
            bit_range: None,
        }),
        (Some(data_bit_offset), None, Some(width)) => {
            if width == 0 {
                return Err(type_unsupported(format!("成员 {} 的位宽为 0", member.name)));
            }
            let byte_offset = data_bit_offset / 8;
            if member
                .byte_offset
                .is_some_and(|declared| declared != byte_offset)
            {
                return Err(type_unsupported(format!(
                    "成员 {} 的 byte offset 与 DW_AT_data_bit_offset 冲突",
                    member.name
                )));
            }
            let lsb = data_bit_offset % 8;
            let bit_end = lsb
                .checked_add(width)
                .ok_or_else(|| type_unsupported("DW_AT_data_bit_offset 位域范围溢出"))?;
            let storage_size = bit_end
                .checked_add(7)
                .map(|bits| bits / 8)
                .ok_or_else(|| type_unsupported("DW_AT_data_bit_offset storage size 溢出"))?;
            let storage_bits = storage_size
                .checked_mul(8)
                .ok_or_else(|| type_unsupported("位域 storage size 溢出"))?;
            let dwarf_bit_offset = storage_bits
                .checked_sub(bit_end)
                .ok_or_else(|| type_unsupported("DW_AT_data_bit_offset 位域范围无效"))?;
            Ok(NormalizedMember {
                byte_offset,
                storage_size: Some(storage_size),
                dwarf_bit_offset: Some(dwarf_bit_offset),
                bit_size: Some(width),
                bit_range: Some(BitRange::new(lsb, width)),
            })
        }
        (None, Some(dwarf_bit_offset), Some(width)) => {
            if width == 0 {
                return Err(type_unsupported(format!("成员 {} 的位宽为 0", member.name)));
            }
            let byte_offset = member
                .byte_offset
                .ok_or_else(|| dynamic_location(&member.name))?;
            let storage_size = member
                .storage_size
                .map_or_else(|| type_size(index, member.type_id), Ok)?;
            let storage_bits = storage_size
                .checked_mul(8)
                .ok_or_else(|| type_unsupported("位域 storage size 溢出"))?;
            let lsb = storage_bits
                .checked_sub(dwarf_bit_offset)
                .and_then(|value| value.checked_sub(width))
                .ok_or_else(|| type_unsupported("DWARF v3/v4 位域 offset 无效"))?;
            Ok(NormalizedMember {
                byte_offset,
                storage_size: Some(storage_size),
                dwarf_bit_offset: Some(dwarf_bit_offset),
                bit_size: Some(width),
                bit_range: Some(BitRange::new(lsb, width)),
            })
        }
        _ => Err(type_unsupported(format!(
            "成员 {} 的位域元数据不完整",
            member.name
        ))),
    }
}

fn layout_from_type(
    index: &DwarfIndex,
    type_id: TypeId,
    visiting: &mut BTreeSet<TypeId>,
) -> Result<AccessLayout, JlinkError> {
    let type_id = unwrap_type(index, type_id)?.type_id;
    if !visiting.insert(type_id) {
        return Err(type_unsupported(format!(
            "递归 value layout 在 {type_id:#x} 处没有指针边界"
        )));
    }
    let result = match index.types.get(&type_id) {
        Some(TypeNode::Base {
            name,
            byte_size,
            encoding,
        }) if *encoding != ScalarEncoding::Other && *byte_size > 0 => Ok(AccessLayout::Scalar {
            name: name.clone(),
            byte_size: *byte_size,
            encoding: *encoding,
        }),
        Some(TypeNode::Base { name, .. }) => Err(type_unsupported(format!(
            "基础类型 {name} 的编码或大小不受 V1 支持"
        ))),
        Some(TypeNode::Pointer { byte_size }) if *byte_size > 0 => Ok(AccessLayout::Pointer {
            byte_size: *byte_size,
        }),
        Some(TypeNode::Pointer { .. }) => Err(type_unsupported("指针大小为 0")),
        Some(TypeNode::Structure { byte_size, members }) => Ok(AccessLayout::Structure {
            byte_size: *byte_size,
            members: layout_members(index, members, visiting)?,
        }),
        Some(TypeNode::Union { byte_size, members }) => Ok(AccessLayout::Union {
            byte_size: *byte_size,
            members: layout_members(index, members, visiting)?,
        }),
        Some(TypeNode::Array { element, count }) => Ok(AccessLayout::Array {
            element: Box::new(layout_from_type(index, *element, visiting)?),
            count: *count,
        }),
        Some(TypeNode::Typedef { .. } | TypeNode::Qualifier { .. }) => unreachable!(),
        None => Err(type_unsupported(format!("DWARF type {type_id:#x} 不存在"))),
    };
    visiting.remove(&type_id);
    result
}

fn layout_members(
    index: &DwarfIndex,
    members: &[Member],
    visiting: &mut BTreeSet<TypeId>,
) -> Result<Vec<AccessMember>, JlinkError> {
    members
        .iter()
        .map(|member| {
            let normalized = normalize_member(index, member)?;
            Ok(AccessMember::new(
                member.name.clone(),
                normalized.byte_offset,
                normalized.storage_size,
                normalized.dwarf_bit_offset,
                normalized.bit_size,
                layout_from_type(index, member.type_id, visiting)?,
            ))
        })
        .collect()
}

fn collect_direct_paths(index: &DwarfIndex) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for (name, variables) in &index.variables {
        let definitions: Vec<&Variable> = variables
            .iter()
            .filter(|variable| !variable.declaration)
            .collect();
        let [variable] = definitions.as_slice() else {
            continue;
        };
        if variable.location == VariableLocation::Dynamic {
            continue;
        }
        collect_paths_for_type(
            index,
            variable.type_id,
            name,
            &mut BTreeSet::new(),
            &mut paths,
        );
    }
    paths.into_iter().collect()
}

fn collect_paths_for_type(
    index: &DwarfIndex,
    type_id: TypeId,
    path: &str,
    visiting: &mut BTreeSet<TypeId>,
    paths: &mut BTreeSet<String>,
) {
    let Ok(unwrapped) = unwrap_type(index, type_id) else {
        return;
    };
    let type_id = unwrapped.type_id;
    if layout_from_type(index, type_id, &mut BTreeSet::new())
        .ok()
        .and_then(|layout| layout.byte_size())
        .is_some()
    {
        paths.insert(path.to_owned());
    }
    if !visiting.insert(type_id) {
        return;
    }
    if let Some(TypeNode::Structure { members, .. } | TypeNode::Union { members, .. }) =
        index.types.get(&type_id)
    {
        for member in members {
            if member.byte_offset.is_none() {
                continue;
            }
            let member_path = format!("{path}.{}", member.name);
            collect_paths_for_type(index, member.type_id, &member_path, visiting, paths);
        }
    }
    visiting.remove(&type_id);
}

fn symbol_not_found(path: impl AsRef<str>) -> JlinkError {
    JlinkError::new(
        ErrorCode::SymbolNotFound,
        format!("DWARF 路径 {} 不存在", path.as_ref()),
        false,
    )
}

fn symbol_ambiguous(path: impl AsRef<str>, candidates: usize) -> JlinkError {
    JlinkError::new(
        ErrorCode::SymbolAmbiguous,
        format!(
            "DWARF 路径 {} 对应 {candidates} 个定义，无法唯一解析",
            path.as_ref()
        ),
        false,
    )
}

fn dynamic_location(path: impl AsRef<str>) -> JlinkError {
    JlinkError::new(
        ErrorCode::DynamicLocationUnsupported,
        format!("DWARF 路径 {} 不是单一静态 DW_OP_addr 位置", path.as_ref()),
        false,
    )
}

fn type_unsupported(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TypeUnsupported, message, false)
}

fn validate_dwarf_versions(versions: &BTreeSet<u16>) -> Result<(), JlinkError> {
    if let Some(version) = versions.iter().find(|version| !matches!(version, 3 | 4)) {
        return Err(type_unsupported(format!(
            "DWARF {version} 未通过 V1 fixture 验证；当前只支持 DWARF 3/4"
        )));
    }
    Ok(())
}

fn slice_required(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::SliceRequired, message, false)
}

fn slice_range_error(bound: Option<u64>, message: impl Into<String>) -> JlinkError {
    if bound.is_none() {
        slice_required(message)
    } else {
        value_invalid(message)
    }
}

fn value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}

fn dwarf_error(context: &str, error: gimli::Error) -> JlinkError {
    value_invalid(format!("{context} 失败：{error}"))
}

fn sha256(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_index(location: VariableLocation) -> DwarfIndex {
        DwarfIndex {
            types: BTreeMap::from([(
                1,
                TypeNode::Base {
                    name: "uint32_t".to_owned(),
                    byte_size: 4,
                    encoding: ScalarEncoding::Unsigned,
                },
            )]),
            variables: BTreeMap::from([(
                "value".to_owned(),
                vec![Variable {
                    type_id: 1,
                    location,
                    declaration: false,
                }],
            )]),
            ..DwarfIndex::default()
        }
    }

    #[test]
    fn t_p2_dwarf_rejects_dynamic_location() {
        let index = scalar_index(VariableLocation::Dynamic);
        let selector = VariableSelector::new("value", None).expect("selector");
        let error = resolve_access_plan(&index, &"0".repeat(64), &selector)
            .expect_err("dynamic location must be rejected");
        assert_eq!(error.code(), ErrorCode::DynamicLocationUnsupported);
    }

    #[test]
    fn t_p2_dwarf_rejects_ambiguous_exact_name() {
        let mut index = scalar_index(VariableLocation::Static(0x2000_0000));
        index
            .variables
            .get_mut("value")
            .expect("variable")
            .push(Variable {
                type_id: 1,
                location: VariableLocation::Static(0x2000_0004),
                declaration: false,
            });
        let selector = VariableSelector::new("value", None).expect("selector");
        let error = resolve_access_plan(&index, &"0".repeat(64), &selector)
            .expect_err("ambiguous name must be rejected");
        assert_eq!(error.code(), ErrorCode::SymbolAmbiguous);
    }

    #[test]
    fn t_p2_dwarf_ignores_declarations_when_one_definition_exists() {
        let mut index = scalar_index(VariableLocation::Static(0x2000_0000));
        index
            .variables
            .get_mut("value")
            .expect("variable")
            .push(Variable {
                type_id: 1,
                location: VariableLocation::Dynamic,
                declaration: true,
            });
        let selector = VariableSelector::new("value", None).expect("selector");
        let plan = resolve_access_plan(&index, &"0".repeat(64), &selector)
            .expect("declaration does not create ambiguity");
        assert_eq!(plan.address(), 0x2000_0000);
    }

    #[test]
    fn t_p2_dwarf_does_not_follow_pointers() {
        let index = DwarfIndex {
            types: BTreeMap::from([(1, TypeNode::Pointer { byte_size: 4 })]),
            variables: BTreeMap::from([(
                "pointer".to_owned(),
                vec![Variable {
                    type_id: 1,
                    location: VariableLocation::Static(0x2000_0000),
                    declaration: false,
                }],
            )]),
            ..DwarfIndex::default()
        };
        let pointer = VariableSelector::new("pointer", None).expect("selector");
        let plan = resolve_access_plan(&index, &"0".repeat(64), &pointer).expect("pointer plan");
        assert_eq!(plan.address(), 0x2000_0000);
        assert_eq!(plan.byte_size(), 4);

        let member = VariableSelector::new("pointer.member", None).expect("selector");
        let error = resolve_access_plan(&index, &"0".repeat(64), &member)
            .expect_err("pointer traversal must be rejected");
        assert_eq!(error.code(), ErrorCode::TypeUnsupported);
    }

    #[test]
    fn t_p2_dwarf_rejects_unverified_dwarf_versions() {
        let error = validate_dwarf_versions(&BTreeSet::from([5]))
            .expect_err("DWARF 5 has no accepted V1 fixture");
        assert_eq!(error.code(), ErrorCode::TypeUnsupported);
        validate_dwarf_versions(&BTreeSet::from([3, 4])).expect("verified versions");
    }

    #[test]
    fn t_p2_dwarf_normalizes_data_bit_offset() {
        let index = DwarfIndex {
            types: BTreeMap::from([
                (
                    1,
                    TypeNode::Base {
                        name: "int8_t".to_owned(),
                        byte_size: 1,
                        encoding: ScalarEncoding::Signed,
                    },
                ),
                (
                    2,
                    TypeNode::Structure {
                        byte_size: 1,
                        members: vec![Member {
                            name: "field".to_owned(),
                            type_id: 1,
                            byte_offset: None,
                            storage_size: None,
                            bit_size: Some(3),
                            dwarf_bit_offset: None,
                            data_bit_offset: Some(0),
                        }],
                    },
                ),
            ]),
            variables: BTreeMap::from([(
                "value".to_owned(),
                vec![Variable {
                    type_id: 2,
                    location: VariableLocation::Static(0x2000_0000),
                    declaration: false,
                }],
            )]),
            ..DwarfIndex::default()
        };
        let selector = VariableSelector::new("value.field", None).expect("selector");
        let plan = resolve_access_plan(&index, &"0".repeat(64), &selector).expect("access plan");
        assert_eq!(plan.address(), 0x2000_0000);
        assert_eq!(plan.byte_size(), 1);
        assert_eq!(plan.bit_range(), Some(BitRange::new(0, 3)));
    }

    #[test]
    fn t_p2_value_reports_unbounded_slice_byte_overflow() {
        let index = DwarfIndex {
            types: BTreeMap::from([
                (
                    1,
                    TypeNode::Base {
                        name: "uint16_t".to_owned(),
                        byte_size: 2,
                        encoding: ScalarEncoding::Unsigned,
                    },
                ),
                (
                    2,
                    TypeNode::Array {
                        element: 1,
                        count: None,
                    },
                ),
            ]),
            ..DwarfIndex::default()
        };
        let slice = ElementSlice::new(0, 1_u64 << 63).expect("valid element range");
        let error = resolve_slice(&index, 2, 0x2000_0000, slice)
            .err()
            .expect("byte range overflows even though element range is valid");
        assert_eq!(error.code(), ErrorCode::SliceRequired);
    }
}
