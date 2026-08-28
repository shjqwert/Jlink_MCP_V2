use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use gimli::{
    AttributeValue, Dwarf, DwarfSections, EndianSlice, EntriesTreeNode, Operation, Reader,
    RunTimeEndian, Unit, UnitType,
};
use object::{Object, ObjectSection};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

type DwarfReader<'a> = EndianSlice<'a, RunTimeEndian>;
type TypeId = u64;

#[derive(Clone, Debug)]
enum TypeNode {
    Base {
        name: String,
        byte_size: u64,
        encoding: BaseEncoding,
    },
    Typedef {
        target: TypeId,
    },
    Qualifier {
        target: TypeId,
    },
    Pointer {
        byte_size: u64,
    },
    Struct {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BaseEncoding {
    Signed,
    Unsigned,
    Float,
    Other,
}

#[derive(Clone, Debug)]
struct Member {
    name: String,
    type_id: TypeId,
    byte_offset: u64,
    storage_size: Option<u64>,
    bit_size: Option<u64>,
    dwarf_bit_offset: Option<u64>,
}

#[derive(Clone, Debug)]
struct Variable {
    type_id: TypeId,
    address: Option<u64>,
}

#[derive(Default)]
struct DwarfIndex {
    types: BTreeMap<TypeId, TypeNode>,
    variables: BTreeMap<String, Variable>,
    producers: BTreeSet<String>,
    versions: BTreeSet<u16>,
    unit_count: u64,
    type_unit_count: u64,
    static_location_count: u64,
    type_signatures: BTreeMap<u64, TypeId>,
}

#[derive(Clone, Copy)]
enum Step<'a> {
    Member(&'a str),
    Index(u64),
    Slice { start: u64, count: u64 },
}

#[derive(Debug)]
struct ResolvedPlan {
    address: u64,
    type_id: TypeId,
    byte_size: u64,
    bit_lsb: Option<u64>,
    bit_size: Option<u64>,
    is_slice: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactEvidence {
    path: String,
    sha256: String,
    byte_length: u64,
    architecture: String,
    little_endian: bool,
    dwarf_versions: Vec<u16>,
    producers: Vec<String>,
    unit_count: u64,
    type_unit_count: u64,
    type_count: u64,
    variable_count: u64,
    static_location_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccessPlanEvidence {
    selector: String,
    address: String,
    byte_size: u64,
    bit_lsb: Option<u64>,
    bit_size: Option<u64>,
    encoding: String,
    decoded: Value,
    expected: Value,
    matches_expected: bool,
    encode_decode_round_trip: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    verdict: &'static str,
    fixture_source: SourceEvidence,
    fixture_artifact: ArtifactEvidence,
    actual_project_artifact: Option<ArtifactEvidence>,
    access_plans: Vec<AccessPlanEvidence>,
    fixed_location_only: bool,
    flexible_array_requires_explicit_slice: bool,
    union_active_member_inferred: bool,
    all_assertions_passed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceEvidence {
    path: String,
    sha256: String,
}

pub fn run() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    ensure!(
        (3..=4).contains(&args.len()),
        "usage: f0c-dwarf <fixture.out> <evidence.json> [actual-project.out]"
    );

    let fixture_path = PathBuf::from(&args[1]);
    let evidence_path = PathBuf::from(&args[2]);
    let actual_path = args.get(3).map(PathBuf::from);
    let fixture_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixture")
        .join("F0cDwarfFixture.c");

    let fixture_data = fs::read(&fixture_path)
        .with_context(|| format!("read fixture {}", fixture_path.display()))?;
    let fixture_object =
        object::File::parse(fixture_data.as_slice()).context("parse fixture ELF")?;
    ensure!(
        fixture_object.is_little_endian(),
        "fixture must be little-endian"
    );

    let fixture_index = parse_dwarf_index(&fixture_object).context("index fixture DWARF")?;
    ensure!(
        fixture_index
            .producers
            .iter()
            .any(|producer| producer.contains("IAR ANSI C/C++ Compiler V8.32.3.193")),
        "fixture producer is not IAR 8.32.3.193"
    );
    ensure!(
        fixture_index.versions == BTreeSet::from([4]),
        "fixture must use DWARF 4"
    );

    let mut plans = Vec::new();
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.stNested.ulSequence",
        "gstF0cRoot",
        &[Step::Member("stNested"), Step::Member("ulSequence")],
        Value::String("7".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.stNested.awMatrix[1][2]",
        "gstF0cRoot",
        &[
            Step::Member("stNested"),
            Step::Member("awMatrix"),
            Step::Index(1),
            Step::Index(2),
        ],
        Value::String("3".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.stFlags.uiReadyFlg",
        "gstF0cRoot",
        &[Step::Member("stFlags"), Step::Member("uiReadyFlg")],
        Value::String("1".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.stFlags.iDelta",
        "gstF0cRoot",
        &[Step::Member("stFlags"), Step::Member("iDelta")],
        Value::String("-7".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.stFlags.uiMode",
        "gstF0cRoot",
        &[Step::Member("stFlags"), Step::Member("uiMode")],
        Value::String("5".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.unPayload.fPhysicalValue",
        "gstF0cRoot",
        &[Step::Member("unPayload"), Step::Member("fPhysicalValue")],
        Value::String("1.0".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cFlex.aucPayload slice(start=1,count=3)",
        "gstF0cFlex",
        &[
            Step::Member("aucPayload"),
            Step::Slice { start: 1, count: 3 },
        ],
        serde_json::json!([22, 33, 44]),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.ullCounter",
        "gstF0cRoot",
        &[Step::Member("ullCounter")],
        Value::String("18364758544493064720".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.llOffset",
        "gstF0cRoot",
        &[Step::Member("llOffset")],
        Value::String("-5124095576030430".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.fFloat",
        "gstF0cRoot",
        &[Step::Member("fFloat")],
        Value::String("1.25".into()),
    )?;
    add_plan(
        &mut plans,
        &fixture_object,
        &fixture_index,
        "gstF0cRoot.dDouble",
        "gstF0cRoot",
        &[Step::Member("dDouble")],
        Value::String("-2.5".into()),
    )?;
    for (array_index, expected) in [(0, "NaN"), (1, "Infinity")] {
        add_plan(
            &mut plans,
            &fixture_object,
            &fixture_index,
            &format!("gaunF0cFloatSpecial[{array_index}].fPhysicalValue"),
            "gaunF0cFloatSpecial",
            &[Step::Index(array_index), Step::Member("fPhysicalValue")],
            Value::String(expected.into()),
        )?;
    }
    for (array_index, expected) in [(0, "NaN"), (1, "Infinity")] {
        add_plan(
            &mut plans,
            &fixture_object,
            &fixture_index,
            &format!("gaunF0cDoubleSpecial[{array_index}].dPhysicalValue"),
            "gaunF0cDoubleSpecial",
            &[Step::Index(array_index), Step::Member("dPhysicalValue")],
            Value::String(expected.into()),
        )?;
    }

    let all_assertions_passed = plans
        .iter()
        .all(|plan| plan.matches_expected && plan.encode_decode_round_trip);
    for plan in plans
        .iter()
        .filter(|plan| !plan.matches_expected || !plan.encode_decode_round_trip)
    {
        eprintln!(
            "failed plan {}: decoded={}, expected={}, roundTrip={}",
            plan.selector, plan.decoded, plan.expected, plan.encode_decode_round_trip
        );
    }
    ensure!(
        all_assertions_passed,
        "one or more access-plan assertions failed"
    );

    let fixture_artifact = artifact_evidence(
        &fixture_path,
        &fixture_data,
        &fixture_object,
        &fixture_index,
    )?;
    let actual_project_artifact = if let Some(path) = actual_path {
        let data =
            fs::read(&path).with_context(|| format!("read actual artifact {}", path.display()))?;
        let object = object::File::parse(data.as_slice()).context("parse actual-project ELF")?;
        let index = parse_dwarf_index(&object).context("index actual-project DWARF")?;
        ensure!(
            !index.types.is_empty(),
            "actual-project artifact contains no indexed types"
        );
        ensure!(
            !index.variables.is_empty(),
            "actual-project artifact contains no indexed variables"
        );
        Some(artifact_evidence(&path, &data, &object, &index)?)
    } else {
        None
    };

    let evidence = Evidence {
        verdict: "PASS",
        fixture_source: SourceEvidence {
            path: fixture_source.display().to_string(),
            sha256: sha256_file(&fixture_source)?,
        },
        fixture_artifact,
        actual_project_artifact,
        access_plans: plans,
        fixed_location_only: true,
        flexible_array_requires_explicit_slice: true,
        union_active_member_inferred: false,
        all_assertions_passed,
    };
    let serialized = serde_json::to_vec_pretty(&evidence).context("serialize evidence")?;
    fs::write(&evidence_path, serialized)
        .with_context(|| format!("write evidence {}", evidence_path.display()))?;
    println!("F0-C PASS: {} access plans", evidence.access_plans.len());
    Ok(())
}

fn parse_dwarf_index(object: &object::File<'_>) -> Result<DwarfIndex> {
    let endian = if object.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let sections = DwarfSections::load(|id| -> Result<Cow<'_, [u8]>, object::Error> {
        match object.section_by_name(id.name()) {
            Some(section) => section.uncompressed_data(),
            None => Ok(Cow::Borrowed(&[])),
        }
    })
    .context("load DWARF sections")?;
    let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
    index_dwarf(&dwarf)
}

fn index_dwarf(dwarf: &Dwarf<DwarfReader<'_>>) -> Result<DwarfIndex> {
    let mut index = DwarfIndex::default();
    let mut type_units = dwarf.type_units();
    while let Some(header) = type_units.next().context("read type-unit header")? {
        if let UnitType::Type {
            type_signature,
            type_offset,
        } = header.type_()
        {
            let type_id = type_offset
                .to_debug_types_offset(&header)
                .context("type-unit definition is not in .debug_types")?
                .0 as u64
                | (1u64 << 63);
            index.type_signatures.insert(type_signature.0, type_id);
        }
    }

    let mut units = dwarf.units();
    while let Some(header) = units.next().context("read unit header")? {
        let unit = dwarf.unit(header).context("read unit")?;
        index.unit_count += 1;
        index.versions.insert(unit.header.version());
        let mut tree = unit.entries_tree(None).context("open unit entry tree")?;
        let root = tree.root().context("read unit root")?;
        process_node(dwarf, &unit, root, None, &mut index)?;
    }

    let mut type_units = dwarf.type_units();
    while let Some(header) = type_units.next().context("read type-unit header")? {
        let unit = dwarf.unit(header).context("read type unit")?;
        index.type_unit_count += 1;
        index.versions.insert(unit.header.version());
        let mut tree = unit
            .entries_tree(None)
            .context("open type-unit entry tree")?;
        let root = tree.root().context("read type-unit root")?;
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
) -> Result<()> {
    let entry = node.entry();
    let tag = entry.tag();
    let id = if let Some(offset) = entry.offset().to_debug_info_offset(&unit.header) {
        offset.0 as u64
    } else {
        entry
            .offset()
            .to_debug_types_offset(&unit.header)
            .context("DIE is not in .debug_info or .debug_types")?
            .0 as u64
            | (1u64 << 63)
    };
    let name = attr_string(dwarf, unit, entry.attr_value(gimli::DW_AT_name)?)?;
    let byte_size = attr_udata(entry.attr_value(gimli::DW_AT_byte_size)?).unwrap_or(0);
    let referenced_type = type_reference(entry.attr_value(gimli::DW_AT_type)?, unit, index)?;

    let mut child_owner = None;
    match tag {
        gimli::DW_TAG_compile_unit => {
            if let Some(producer) =
                attr_string(dwarf, unit, entry.attr_value(gimli::DW_AT_producer)?)?
            {
                index.producers.insert(producer);
            }
        }
        gimli::DW_TAG_base_type => {
            let encoding = match entry.attr_value(gimli::DW_AT_encoding)? {
                Some(AttributeValue::Encoding(value)) if value == gimli::DW_ATE_signed => {
                    BaseEncoding::Signed
                }
                Some(AttributeValue::Encoding(value)) if value == gimli::DW_ATE_signed_char => {
                    BaseEncoding::Signed
                }
                Some(AttributeValue::Encoding(value)) if value == gimli::DW_ATE_unsigned => {
                    BaseEncoding::Unsigned
                }
                Some(AttributeValue::Encoding(value)) if value == gimli::DW_ATE_unsigned_char => {
                    BaseEncoding::Unsigned
                }
                Some(AttributeValue::Encoding(value)) if value == gimli::DW_ATE_float => {
                    BaseEncoding::Float
                }
                _ => BaseEncoding::Other,
            };
            index.types.insert(
                id,
                TypeNode::Base {
                    name: name.unwrap_or_else(|| "<anonymous-base>".into()),
                    byte_size,
                    encoding,
                },
            );
        }
        gimli::DW_TAG_typedef => {
            if let Some(target) = referenced_type {
                index.types.insert(id, TypeNode::Typedef { target });
            }
        }
        gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => {
            if let Some(target) = referenced_type {
                index.types.insert(id, TypeNode::Qualifier { target });
            }
        }
        gimli::DW_TAG_pointer_type => {
            index.types.insert(
                id,
                TypeNode::Pointer {
                    byte_size: if 0 == byte_size {
                        u64::from(unit.header.address_size())
                    } else {
                        byte_size
                    },
                },
            );
        }
        gimli::DW_TAG_structure_type => {
            index.types.insert(
                id,
                TypeNode::Struct {
                    byte_size,
                    members: Vec::new(),
                },
            );
            child_owner = Some(id);
        }
        gimli::DW_TAG_union_type => {
            index.types.insert(
                id,
                TypeNode::Union {
                    byte_size,
                    members: Vec::new(),
                },
            );
            child_owner = Some(id);
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
                child_owner = Some(id);
            }
        }
        gimli::DW_TAG_subrange_type => {
            if let Some(TypeNode::Array { count, .. }) =
                owner.and_then(|owner_id| index.types.get_mut(&owner_id))
            {
                let upper = attr_udata(entry.attr_value(gimli::DW_AT_upper_bound)?);
                *count = upper.and_then(|value| {
                    if value == u64::from(u32::MAX) {
                        None
                    } else {
                        value.checked_add(1)
                    }
                });
            }
        }
        gimli::DW_TAG_member => {
            if let (Some(owner_id), Some(name), Some(type_id)) = (owner, name, referenced_type) {
                let byte_offset =
                    member_offset(entry.attr_value(gimli::DW_AT_data_member_location)?, unit)?;
                let member = Member {
                    name,
                    type_id,
                    byte_offset,
                    storage_size: attr_udata(entry.attr_value(gimli::DW_AT_byte_size)?),
                    bit_size: attr_udata(entry.attr_value(gimli::DW_AT_bit_size)?),
                    dwarf_bit_offset: attr_udata(entry.attr_value(gimli::DW_AT_bit_offset)?),
                };
                match index.types.get_mut(&owner_id) {
                    Some(TypeNode::Struct { members, .. })
                    | Some(TypeNode::Union { members, .. }) => members.push(member),
                    _ => bail!("member owner {owner_id:#x} is not an aggregate"),
                }
            }
        }
        gimli::DW_TAG_variable => {
            if let (Some(name), Some(type_id)) = (name, referenced_type) {
                let address = static_address(entry.attr_value(gimli::DW_AT_location)?, unit)?;
                if address.is_some() {
                    index.static_location_count += 1;
                }
                index.variables.insert(name, Variable { type_id, address });
            }
        }
        _ => {}
    }

    let mut children = node.children();
    while let Some(child) = children.next().context("read child DIE")? {
        process_node(dwarf, unit, child, child_owner, index)?;
    }
    Ok(())
}

fn attr_string<R: Reader<Offset = usize>>(
    dwarf: &Dwarf<R>,
    unit: &Unit<R>,
    value: Option<AttributeValue<R>>,
) -> Result<Option<String>> {
    value
        .map(|value| {
            dwarf
                .attr_string(unit, value)
                .context("resolve DWARF string")?
                .to_string_lossy()
                .context("decode DWARF string")
                .map(|value| value.into_owned())
        })
        .transpose()
}

fn attr_udata<R: Reader>(value: Option<AttributeValue<R>>) -> Option<u64> {
    value.and_then(|value| value.udata_value())
}

fn type_reference<R: Reader<Offset = usize>>(
    value: Option<AttributeValue<R>>,
    unit: &Unit<R>,
    index: &DwarfIndex,
) -> Result<Option<TypeId>> {
    match value {
        Some(AttributeValue::UnitRef(offset)) => {
            if let Some(offset) = offset.to_debug_info_offset(&unit.header) {
                Ok(Some(offset.0 as u64))
            } else {
                Ok(Some(
                    offset
                        .to_debug_types_offset(&unit.header)
                        .context("unit-relative type reference has no section")?
                        .0 as u64
                        | (1u64 << 63),
                ))
            }
        }
        Some(AttributeValue::DebugInfoRef(offset)) => Ok(Some(offset.0 as u64)),
        Some(AttributeValue::DebugTypesRef(signature)) => Ok(Some(
            *index
                .type_signatures
                .get(&signature.0)
                .with_context(|| format!("type signature {:#x} not found", signature.0))?,
        )),
        None => Ok(None),
        Some(other) => bail!("unsupported type reference {other:?}"),
    }
}

fn member_offset<R: Reader>(value: Option<AttributeValue<R>>, unit: &Unit<R>) -> Result<u64> {
    match value {
        Some(value) if value.udata_value().is_some() => Ok(value.udata_value().unwrap_or(0)),
        Some(AttributeValue::Exprloc(expression)) => {
            let mut operations = expression.operations(unit.encoding());
            match operations.next().context("decode member location")? {
                Some(Operation::PlusConstant { value }) => {
                    ensure!(
                        operations.next()?.is_none(),
                        "member location has multiple operations"
                    );
                    Ok(value)
                }
                other => bail!("unsupported member location {other:?}"),
            }
        }
        None => Ok(0),
        Some(other) => bail!("unsupported member location attribute {other:?}"),
    }
}

fn static_address<R: Reader>(
    value: Option<AttributeValue<R>>,
    unit: &Unit<R>,
) -> Result<Option<u64>> {
    match value {
        Some(AttributeValue::Exprloc(expression)) => {
            let mut operations = expression.operations(unit.encoding());
            match operations.next().context("decode variable location")? {
                Some(Operation::Address { address }) => {
                    ensure!(
                        operations.next()?.is_none(),
                        "variable location has multiple operations"
                    );
                    Ok(Some(address))
                }
                _ => Ok(None),
            }
        }
        Some(AttributeValue::Addr(address)) => Ok(Some(address)),
        _ => Ok(None),
    }
}

fn add_plan(
    plans: &mut Vec<AccessPlanEvidence>,
    object: &object::File<'_>,
    index: &DwarfIndex,
    selector: &str,
    variable: &str,
    steps: &[Step<'_>],
    expected: Value,
) -> Result<()> {
    let plan = resolve_plan(index, variable, steps)
        .with_context(|| format!("resolve selector {selector}"))?;
    let bytes = read_memory(object, plan.address, plan.byte_size)
        .with_context(|| format!("read selector {selector}"))?;
    let (decoded, encoding, round_trip) = decode_plan(index, &plan, &bytes)?;
    let matches_expected = decoded == expected;
    plans.push(AccessPlanEvidence {
        selector: selector.into(),
        address: format!("0x{:08X}", plan.address),
        byte_size: plan.byte_size,
        bit_lsb: plan.bit_lsb,
        bit_size: plan.bit_size,
        encoding,
        decoded,
        expected,
        matches_expected,
        encode_decode_round_trip: round_trip,
    });
    Ok(())
}

fn resolve_plan(
    index: &DwarfIndex,
    variable_name: &str,
    steps: &[Step<'_>],
) -> Result<ResolvedPlan> {
    let variable = index
        .variables
        .get(variable_name)
        .with_context(|| format!("variable {variable_name} not found"))?;
    let mut address = variable
        .address
        .context("variable does not have a fixed DW_OP_addr location")?;
    let mut type_id = variable.type_id;
    let mut bit_lsb = None;
    let mut bit_size = None;
    let mut is_slice = false;
    let mut selected_size = None;

    for step in steps {
        type_id = unwrap_qualifiers(index, type_id)?;
        match *step {
            Step::Member(name) => {
                let member = match index.types.get(&type_id) {
                    Some(TypeNode::Struct { members, .. })
                    | Some(TypeNode::Union { members, .. }) => members
                        .iter()
                        .find(|member| member.name == name)
                        .with_context(|| format!("member {name} not found"))?,
                    other => bail!("member step applied to {other:?}"),
                };
                address = address
                    .checked_add(member.byte_offset)
                    .context("member address overflow")?;
                type_id = member.type_id;
                if let (Some(width), Some(dwarf_offset), Some(storage_size)) = (
                    member.bit_size,
                    member.dwarf_bit_offset,
                    member.storage_size,
                ) {
                    let storage_bits = storage_size
                        .checked_mul(8)
                        .context("bit storage overflow")?;
                    bit_lsb = Some(
                        storage_bits
                            .checked_sub(dwarf_offset)
                            .and_then(|value| value.checked_sub(width))
                            .context("invalid DWARF v4 bit offset")?,
                    );
                    bit_size = Some(width);
                    selected_size = Some(storage_size);
                }
            }
            Step::Index(element_index) => {
                let (element, count) = match index.types.get(&type_id) {
                    Some(TypeNode::Array { element, count }) => (*element, *count),
                    other => bail!("index step applied to {other:?}"),
                };
                if let Some(count) = count {
                    ensure!(
                        element_index < count,
                        "array index {element_index} is out of range {count}"
                    );
                }
                let element_size = type_size(index, element)?;
                address = address
                    .checked_add(
                        element_index
                            .checked_mul(element_size)
                            .context("index offset overflow")?,
                    )
                    .context("index address overflow")?;
                type_id = element;
            }
            Step::Slice { start, count } => {
                let element = match index.types.get(&type_id) {
                    Some(TypeNode::Array {
                        element,
                        count: bound,
                    }) => {
                        if let Some(bound) = bound {
                            ensure!(
                                start.checked_add(count).context("slice range overflow")? <= *bound,
                                "slice is out of range"
                            );
                        }
                        *element
                    }
                    other => bail!("slice step applied to {other:?}"),
                };
                let element_size = type_size(index, element)?;
                address = address
                    .checked_add(
                        start
                            .checked_mul(element_size)
                            .context("slice offset overflow")?,
                    )
                    .context("slice address overflow")?;
                type_id = element;
                selected_size = Some(
                    count
                        .checked_mul(element_size)
                        .context("slice size overflow")?,
                );
                is_slice = true;
            }
        }
    }

    let byte_size = selected_size.unwrap_or(type_size(index, type_id)?);
    Ok(ResolvedPlan {
        address,
        type_id,
        byte_size,
        bit_lsb,
        bit_size,
        is_slice,
    })
}

fn unwrap_qualifiers(index: &DwarfIndex, mut type_id: TypeId) -> Result<TypeId> {
    let mut visited = BTreeSet::new();
    loop {
        ensure!(
            visited.insert(type_id),
            "cyclic type reference at {type_id:#x}"
        );
        match index.types.get(&type_id) {
            Some(TypeNode::Typedef { target }) | Some(TypeNode::Qualifier { target }) => {
                type_id = *target
            }
            Some(_) => return Ok(type_id),
            None => bail!("type {type_id:#x} not found"),
        }
    }
}

fn type_size(index: &DwarfIndex, type_id: TypeId) -> Result<u64> {
    let type_id = unwrap_qualifiers(index, type_id)?;
    match index.types.get(&type_id) {
        Some(TypeNode::Base { byte_size, .. })
        | Some(TypeNode::Struct { byte_size, .. })
        | Some(TypeNode::Union { byte_size, .. })
        | Some(TypeNode::Pointer { byte_size }) => Ok(*byte_size),
        Some(TypeNode::Array {
            element,
            count: Some(count),
        }) => count
            .checked_mul(type_size(index, *element)?)
            .context("array byte size overflow"),
        Some(TypeNode::Array { count: None, .. }) => {
            bail!("flexible array requires an explicit slice")
        }
        Some(TypeNode::Typedef { .. }) | Some(TypeNode::Qualifier { .. }) => unreachable!(),
        None => bail!("type {type_id:#x} not found"),
    }
}

fn base_type(index: &DwarfIndex, type_id: TypeId) -> Result<(&str, u64, BaseEncoding)> {
    let type_id = unwrap_qualifiers(index, type_id)?;
    match index.types.get(&type_id) {
        Some(TypeNode::Base {
            name,
            byte_size,
            encoding,
        }) => Ok((name, *byte_size, *encoding)),
        other => bail!("resolved value is not a base type: {other:?}"),
    }
}

fn read_memory(object: &object::File<'_>, address: u64, byte_size: u64) -> Result<Vec<u8>> {
    let end = address
        .checked_add(byte_size)
        .context("read range overflow")?;
    for section in object.sections() {
        let section_start = section.address();
        let section_end = section_start
            .checked_add(section.size())
            .context("section range overflow")?;
        if section_start <= address && end <= section_end {
            let data = section.data().context("read ELF section data")?;
            let offset =
                usize::try_from(address - section_start).context("section offset too large")?;
            let length = usize::try_from(byte_size).context("read length too large")?;
            return data
                .get(offset..offset + length)
                .map(ToOwned::to_owned)
                .context("ELF section data is shorter than its address range");
        }
    }
    bail!("address range {address:#x}..{end:#x} is not present in an ELF section")
}

fn decode_plan(
    index: &DwarfIndex,
    plan: &ResolvedPlan,
    bytes: &[u8],
) -> Result<(Value, String, bool)> {
    if plan.is_slice {
        let decoded = Value::Array(bytes.iter().map(|value| Value::from(*value)).collect());
        return Ok((decoded, "byte-slice".into(), true));
    }

    let (name, byte_size, encoding) = base_type(index, plan.type_id)?;
    ensure!(
        usize::try_from(byte_size)? <= bytes.len(),
        "insufficient bytes for {name}"
    );
    if let (Some(lsb), Some(width)) = (plan.bit_lsb, plan.bit_size) {
        let raw = read_unsigned(bytes)?;
        let mask = if 64 == width {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let field = (raw >> lsb) & mask;
        let decoded = if BaseEncoding::Signed == encoding {
            Value::String(sign_extend(field, width).to_string())
        } else {
            Value::String(field.to_string())
        };
        let encoded_field = if BaseEncoding::Signed == encoding {
            (sign_extend(field, width) as u64) & mask
        } else {
            field
        };
        let encoded_raw = (raw & !(mask << lsb)) | (encoded_field << lsb);
        return Ok((decoded, format!("bitfield<{name}>"), encoded_raw == raw));
    }

    match encoding {
        BaseEncoding::Unsigned => {
            let width = usize::try_from(byte_size)?;
            let value = read_unsigned(&bytes[..width])?;
            let encoded = value.to_le_bytes();
            Ok((
                Value::String(value.to_string()),
                format!("unsigned<{name}>"),
                &encoded[..width] == bytes,
            ))
        }
        BaseEncoding::Signed => {
            let width = usize::try_from(byte_size)?;
            let raw = read_unsigned(&bytes[..width])?;
            let value = sign_extend(raw, byte_size * 8);
            let encoded = (value as u64).to_le_bytes();
            Ok((
                Value::String(value.to_string()),
                format!("signed<{name}>"),
                &encoded[..width] == bytes,
            ))
        }
        BaseEncoding::Float if 4 == byte_size => {
            let bits = u32::from_le_bytes(bytes[..4].try_into()?);
            let value = f32::from_bits(bits);
            Ok((
                Value::String(format_float(f64::from(value))),
                format!("float<{name}>"),
                value.to_bits() == bits,
            ))
        }
        BaseEncoding::Float if 8 == byte_size => {
            let bits = u64::from_le_bytes(bytes[..8].try_into()?);
            let value = f64::from_bits(bits);
            Ok((
                Value::String(format_float(value)),
                format!("float<{name}>"),
                value.to_bits() == bits,
            ))
        }
        other => bail!("unsupported base encoding {other:?} with size {byte_size}"),
    }
}

fn read_unsigned(bytes: &[u8]) -> Result<u64> {
    ensure!(
        (1..=8).contains(&bytes.len()),
        "integer width must be 1..=8 bytes"
    );
    let mut padded = [0u8; 8];
    padded[..bytes.len()].copy_from_slice(bytes);
    Ok(u64::from_le_bytes(padded))
}

fn sign_extend(value: u64, width: u64) -> i64 {
    if 64 == width {
        value as i64
    } else {
        let shift = 64 - width;
        ((value << shift) as i64) >> shift
    }
}

fn format_float(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "Infinity".into()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        format!("{value:?}")
    }
}

fn artifact_evidence(
    path: &Path,
    data: &[u8],
    object: &object::File<'_>,
    index: &DwarfIndex,
) -> Result<ArtifactEvidence> {
    Ok(ArtifactEvidence {
        path: path.display().to_string(),
        sha256: sha256_bytes(data),
        byte_length: u64::try_from(data.len())?,
        architecture: format!("{:?}", object.architecture()),
        little_endian: object.is_little_endian(),
        dwarf_versions: index.versions.iter().copied().collect(),
        producers: index.producers.iter().cloned().collect(),
        unit_count: index.unit_count,
        type_unit_count: index.type_unit_count,
        type_count: u64::try_from(index.types.len())?,
        variable_count: u64::try_from(index.variables.len())?,
        static_location_count: index.static_location_count,
    })
}

fn sha256_file(path: &Path) -> Result<String> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(sha256_bytes(&data))
}

fn sha256_bytes(data: &[u8]) -> String {
    format!("{:X}", Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::{format_float, sign_extend};

    #[test]
    fn sign_extends_bit_fields() {
        assert_eq!(-7, sign_extend(0b1_1001, 5));
        assert_eq!(5, sign_extend(0b0_0101, 5));
    }

    #[test]
    fn formats_non_finite_values_for_json() {
        assert_eq!("NaN", format_float(f64::NAN));
        assert_eq!("Infinity", format_float(f64::INFINITY));
        assert_eq!("-Infinity", format_float(f64::NEG_INFINITY));
    }
}
