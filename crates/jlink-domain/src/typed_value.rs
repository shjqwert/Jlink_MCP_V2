use serde_json::{Map, Number, Value, json};

use crate::{
    AccessLayout, AccessMember, AccessPlan, BitRange, ErrorCode, JlinkError, ScalarEncoding,
};

const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Decodes one complete little-endian ARM memory image through an immutable access plan.
///
/// # Errors
///
/// Returns [`ErrorCode::ValueInvalid`] when `data` does not match the plan size, or
/// [`ErrorCode::TypeUnsupported`] when the retained DWARF layout cannot be represented
/// by the closed V1 `TypedValue` contract.
pub fn decode_typed_value(plan: &AccessPlan, data: &[u8]) -> Result<Value, JlinkError> {
    require_length(data.len(), plan.byte_size(), "$")?;
    decode_layout(plan.layout(), data, plan.bit_range(), "$")
}

/// Validates and encodes one complete V1 `TypedValue` without mutating caller storage.
///
/// `current` is copied before any value is applied. This preserves structure padding,
/// inactive union bytes, and neighboring bit-fields while guaranteeing that a failed
/// compound validation cannot expose partially encoded bytes to the device executor.
///
/// # Errors
///
/// Returns [`ErrorCode::ValueInvalid`] with a `path` detail for shape, type, range, or
/// storage mismatches, or [`ErrorCode::TypeUnsupported`] for a layout outside V1.
pub fn encode_typed_value(
    plan: &AccessPlan,
    current: &[u8],
    value: &Value,
) -> Result<Vec<u8>, JlinkError> {
    require_length(current.len(), plan.byte_size(), "$")?;
    let mut encoded = current.to_vec();
    encode_layout(plan.layout(), &mut encoded, plan.bit_range(), value, "$")?;
    Ok(encoded)
}

pub(crate) fn decode_layout(
    layout: &AccessLayout,
    data: &[u8],
    bit_range: Option<BitRange>,
    path: &str,
) -> Result<Value, JlinkError> {
    match layout {
        AccessLayout::Scalar {
            byte_size,
            encoding,
            ..
        } => decode_scalar(*byte_size, *encoding, data, bit_range, path),
        AccessLayout::Pointer { byte_size } => {
            reject_bit_range(bit_range, path)?;
            require_length(data.len(), *byte_size, path)?;
            let address = read_unsigned(data, path)?;
            Ok(json!({ "$pointer": format!("{address:#x}") }))
        }
        AccessLayout::Structure { byte_size, members } => {
            reject_bit_range(bit_range, path)?;
            require_length(data.len(), *byte_size, path)?;
            let mut object = Map::new();
            for member in members {
                let member_path = member_path(path, member.name());
                let (start, end, member_bits) = member_bounds(member, data.len(), &member_path)?;
                object.insert(
                    member.name().to_owned(),
                    decode_layout(
                        member.layout(),
                        &data[start..end],
                        member_bits,
                        &member_path,
                    )?,
                );
            }
            Ok(Value::Object(object))
        }
        AccessLayout::Union { byte_size, members } => {
            reject_bit_range(bit_range, path)?;
            require_length(data.len(), *byte_size, path)?;
            let mut alternatives = Map::new();
            for member in members {
                let member_path = union_member_path(path, member.name());
                let decoded = member_bounds(member, data.len(), &member_path).and_then(
                    |(start, end, member_bits)| {
                        decode_layout(
                            member.layout(),
                            &data[start..end],
                            member_bits,
                            &member_path,
                        )
                    },
                );
                match decoded {
                    Ok(value) => {
                        alternatives.insert(member.name().to_owned(), value);
                    }
                    Err(error) if error.code() == ErrorCode::TypeUnsupported => {}
                    Err(error) => return Err(error),
                }
            }
            if alternatives.is_empty() {
                return Err(type_unsupported(path, "union 没有可按 V1 解释的成员"));
            }
            Ok(json!({ "$union": alternatives }))
        }
        AccessLayout::Array {
            element,
            count: Some(count),
        } => {
            reject_bit_range(bit_range, path)?;
            let element_size = bounded_layout_size(element, path)?;
            let expected = element_size
                .checked_mul(*count)
                .ok_or_else(|| type_unsupported(path, "数组字节长度溢出"))?;
            require_length(data.len(), expected, path)?;
            let element_size = to_usize(element_size, path)?;
            let count = to_usize(*count, path)?;
            let mut values = Vec::with_capacity(count);
            for index in 0..count {
                let start = index
                    .checked_mul(element_size)
                    .ok_or_else(|| type_unsupported(path, "数组元素 offset 溢出"))?;
                let end = start
                    .checked_add(element_size)
                    .ok_or_else(|| type_unsupported(path, "数组元素范围溢出"))?;
                values.push(decode_layout(
                    element,
                    &data[start..end],
                    None,
                    &index_path(path, index),
                )?);
            }
            Ok(Value::Array(values))
        }
        AccessLayout::Array { count: None, .. } => Err(type_unsupported(
            path,
            "无界数组不能在没有独立 slice 的情况下编解码",
        )),
    }
}

fn encode_layout(
    layout: &AccessLayout,
    data: &mut [u8],
    bit_range: Option<BitRange>,
    value: &Value,
    path: &str,
) -> Result<(), JlinkError> {
    match layout {
        AccessLayout::Scalar {
            byte_size,
            encoding,
            ..
        } => encode_scalar(*byte_size, *encoding, data, bit_range, value, path),
        AccessLayout::Pointer { byte_size } => {
            reject_bit_range(bit_range, path)?;
            require_length(data.len(), *byte_size, path)?;
            let address = parse_pointer(value, *byte_size, path)?;
            write_unsigned(data, address, path)
        }
        AccessLayout::Structure { byte_size, members } => {
            encode_structure(*byte_size, members, data, bit_range, value, path)
        }
        AccessLayout::Union { byte_size, members } => {
            reject_bit_range(bit_range, path)?;
            require_length(data.len(), *byte_size, path)?;
            let object = require_object(value, path, "union 值必须是 object")?;
            if object.len() != 1 {
                return Err(value_invalid(path, "union 外层必须只包含 $union"));
            }
            let alternatives = object
                .get("$union")
                .ok_or_else(|| value_invalid(path, "union 值缺少 $union"))?;
            let alternatives =
                require_object(alternatives, path, "$union 必须包含一个成员 object")?;
            if alternatives.len() != 1 {
                return Err(value_invalid(path, "union 写入必须且只能指定一个成员"));
            }
            let (name, member_value) = alternatives
                .iter()
                .next()
                .expect("one union member was required");
            let member = members
                .iter()
                .find(|member| member.name() == name)
                .ok_or_else(|| value_invalid(path, format!("union 不包含成员 {name}")))?;
            let member_path = union_member_path(path, name);
            let (start, end, member_bits) = member_bounds(member, data.len(), &member_path)?;
            encode_layout(
                member.layout(),
                &mut data[start..end],
                member_bits,
                member_value,
                &member_path,
            )
        }
        AccessLayout::Array {
            element,
            count: Some(count),
        } => {
            reject_bit_range(bit_range, path)?;
            let element_size = bounded_layout_size(element, path)?;
            let expected = element_size
                .checked_mul(*count)
                .ok_or_else(|| type_unsupported(path, "数组字节长度溢出"))?;
            require_length(data.len(), expected, path)?;
            let values = value
                .as_array()
                .ok_or_else(|| value_invalid(path, "数组值必须是 JSON array"))?;
            let count = to_usize(*count, path)?;
            if values.len() != count {
                return Err(value_invalid(
                    path,
                    format!("数组需要 {count} 个元素，实际为 {}", values.len()),
                ));
            }
            let element_size = to_usize(element_size, path)?;
            for (index, element_value) in values.iter().enumerate() {
                let start = index
                    .checked_mul(element_size)
                    .ok_or_else(|| type_unsupported(path, "数组元素 offset 溢出"))?;
                let end = start
                    .checked_add(element_size)
                    .ok_or_else(|| type_unsupported(path, "数组元素范围溢出"))?;
                encode_layout(
                    element,
                    &mut data[start..end],
                    None,
                    element_value,
                    &index_path(path, index),
                )?;
            }
            Ok(())
        }
        AccessLayout::Array { count: None, .. } => Err(type_unsupported(
            path,
            "无界数组不能在没有独立 slice 的情况下编解码",
        )),
    }
}

fn encode_structure(
    byte_size: u64,
    members: &[AccessMember],
    data: &mut [u8],
    bit_range: Option<BitRange>,
    value: &Value,
    path: &str,
) -> Result<(), JlinkError> {
    reject_bit_range(bit_range, path)?;
    require_length(data.len(), byte_size, path)?;
    let object = require_object(value, path, "结构体值必须是 object")?;
    validate_member_set(object, members, path)?;
    for member in members {
        let member_path = member_path(path, member.name());
        let (start, end, member_bits) = member_bounds(member, data.len(), &member_path)?;
        let member_value = object
            .get(member.name())
            .expect("member set was validated before encoding");
        encode_layout(
            member.layout(),
            &mut data[start..end],
            member_bits,
            member_value,
            &member_path,
        )?;
    }
    Ok(())
}

fn decode_scalar(
    byte_size: u64,
    encoding: ScalarEncoding,
    data: &[u8],
    bit_range: Option<BitRange>,
    path: &str,
) -> Result<Value, JlinkError> {
    require_length(data.len(), byte_size, path)?;
    match encoding {
        ScalarEncoding::Signed => {
            let (raw, bits) = selected_integer(data, bit_range, path)?;
            Ok(signed_json(sign_extend(raw, bits, path)?, bits))
        }
        ScalarEncoding::Unsigned => {
            let (raw, bits) = selected_integer(data, bit_range, path)?;
            Ok(unsigned_json(raw, bits))
        }
        ScalarEncoding::Boolean => {
            let (raw, _) = selected_integer(data, bit_range, path)?;
            Ok(Value::Bool(raw != 0))
        }
        ScalarEncoding::Float => {
            reject_bit_range(bit_range, path)?;
            decode_float(data, path)
        }
        ScalarEncoding::Other => Err(type_unsupported(path, "基础类型编码不受 V1 支持")),
    }
}

fn encode_scalar(
    byte_size: u64,
    encoding: ScalarEncoding,
    data: &mut [u8],
    bit_range: Option<BitRange>,
    value: &Value,
    path: &str,
) -> Result<(), JlinkError> {
    require_length(data.len(), byte_size, path)?;
    match encoding {
        ScalarEncoding::Signed => {
            let bits = logical_bits(data, bit_range, path)?;
            let signed = parse_signed(value, bits, path)?;
            insert_integer(data, bit_range, signed_to_raw(signed, bits, path)?, path)
        }
        ScalarEncoding::Unsigned => {
            let bits = logical_bits(data, bit_range, path)?;
            let unsigned = parse_unsigned(value, bits, path)?;
            insert_integer(data, bit_range, unsigned, path)
        }
        ScalarEncoding::Boolean => {
            let boolean = value
                .as_bool()
                .ok_or_else(|| value_invalid(path, "布尔值必须是 JSON boolean"))?;
            insert_integer(data, bit_range, u64::from(boolean), path)
        }
        ScalarEncoding::Float => {
            reject_bit_range(bit_range, path)?;
            encode_float(data, value, path)
        }
        ScalarEncoding::Other => Err(type_unsupported(path, "基础类型编码不受 V1 支持")),
    }
}

fn decode_float(data: &[u8], path: &str) -> Result<Value, JlinkError> {
    let value = match data.len() {
        4 => f64::from(f32::from_le_bytes(
            data.try_into()
                .map_err(|_| value_invalid(path, "f32 字节长度无效"))?,
        )),
        8 => f64::from_le_bytes(
            data.try_into()
                .map_err(|_| value_invalid(path, "f64 字节长度无效"))?,
        ),
        _ => {
            return Err(type_unsupported(
                path,
                "V1 只支持 32-bit 和 64-bit IEEE-754 浮点",
            ));
        }
    };
    if value.is_nan() {
        Ok(json!({ "$float": "nan" }))
    } else if value == f64::INFINITY {
        Ok(json!({ "$float": "inf" }))
    } else if value == f64::NEG_INFINITY {
        Ok(json!({ "$float": "-inf" }))
    } else {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| value_invalid(path, "有限浮点无法表示为 JSON number"))
    }
}

fn encode_float(data: &mut [u8], value: &Value, path: &str) -> Result<(), JlinkError> {
    let value = if let Some(number) = value.as_f64() {
        number
    } else {
        let object = require_object(value, path, "浮点值必须是 number 或 $float object")?;
        if object.len() != 1 {
            return Err(value_invalid(path, "$float object 不能包含其他字段"));
        }
        match object.get("$float").and_then(Value::as_str) {
            Some("nan") => f64::NAN,
            Some("inf") => f64::INFINITY,
            Some("-inf") => f64::NEG_INFINITY,
            _ => return Err(value_invalid(path, "$float 必须是 nan、inf 或 -inf")),
        }
    };
    match data.len() {
        4 => {
            let narrowed = narrow_f32(value, path)?;
            data.copy_from_slice(&narrowed.to_le_bytes());
            Ok(())
        }
        8 => {
            data.copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
        _ => Err(type_unsupported(
            path,
            "V1 只支持 32-bit 和 64-bit IEEE-754 浮点",
        )),
    }
}

fn narrow_f32(value: f64, path: &str) -> Result<f32, JlinkError> {
    if value.is_finite() && !(f64::from(f32::MIN)..=f64::from(f32::MAX)).contains(&value) {
        return Err(value_invalid(path, "浮点值超出 f32 范围"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = value as f32;
    Ok(narrowed)
}

fn selected_integer(
    data: &[u8],
    bit_range: Option<BitRange>,
    path: &str,
) -> Result<(u64, u64), JlinkError> {
    let raw = read_unsigned(data, path)?;
    let bits = logical_bits(data, bit_range, path)?;
    Ok(match bit_range {
        Some(range) => ((raw >> range.lsb()) & bit_mask(bits, path)?, bits),
        None => (raw, bits),
    })
}

fn insert_integer(
    data: &mut [u8],
    bit_range: Option<BitRange>,
    value: u64,
    path: &str,
) -> Result<(), JlinkError> {
    match bit_range {
        Some(range) => {
            let bits = logical_bits(data, bit_range, path)?;
            let value_mask = bit_mask(bits, path)?;
            if value & !value_mask != 0 {
                return Err(value_invalid(path, "位域值超出逻辑宽度"));
            }
            let shifted_mask = value_mask
                .checked_shl(
                    u32::try_from(range.lsb())
                        .map_err(|_| type_unsupported(path, "位域 lsb 超出移位范围"))?,
                )
                .ok_or_else(|| type_unsupported(path, "位域 mask 移位溢出"))?;
            let current = read_unsigned(data, path)?;
            write_unsigned(
                data,
                (current & !shifted_mask) | ((value << range.lsb()) & shifted_mask),
                path,
            )
        }
        None => write_unsigned(data, value, path),
    }
}

fn logical_bits(data: &[u8], bit_range: Option<BitRange>, path: &str) -> Result<u64, JlinkError> {
    let storage_bits = u64::try_from(data.len())
        .ok()
        .and_then(|length| length.checked_mul(8))
        .ok_or_else(|| type_unsupported(path, "存储位宽溢出"))?;
    if storage_bits == 0 || storage_bits > 64 {
        return Err(type_unsupported(path, "V1 标量存储宽度必须为 1..64 bit"));
    }
    match bit_range {
        Some(range)
            if range.width() > 0
                && range
                    .lsb()
                    .checked_add(range.width())
                    .is_some_and(|end| end <= storage_bits) =>
        {
            Ok(range.width())
        }
        Some(_) => Err(type_unsupported(path, "位域范围超出存储单元")),
        None => Ok(storage_bits),
    }
}

fn parse_signed(value: &Value, bits: u64, path: &str) -> Result<i128, JlinkError> {
    let parsed = if let Some(number) = value.as_i64() {
        if number.unsigned_abs() > JSON_SAFE_INTEGER {
            return Err(value_invalid(path, "超出 JSON 安全范围的整数必须使用 $int"));
        }
        i128::from(number)
    } else {
        let decimal = integer_tag(value, bits, true, path)?;
        decimal
            .parse::<i128>()
            .map_err(|_| value_invalid(path, "$int 不是有效十进制有符号整数"))?
    };
    let minimum = -(1_i128 << (bits - 1));
    let maximum = (1_i128 << (bits - 1)) - 1;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(value_invalid(
            path,
            format!("有符号整数超出 {bits}-bit 范围"),
        ));
    }
    Ok(parsed)
}

fn parse_unsigned(value: &Value, bits: u64, path: &str) -> Result<u64, JlinkError> {
    let parsed = if let Some(number) = value.as_u64() {
        if number > JSON_SAFE_INTEGER {
            return Err(value_invalid(path, "超出 JSON 安全范围的整数必须使用 $int"));
        }
        u128::from(number)
    } else {
        let decimal = integer_tag(value, bits, false, path)?;
        decimal
            .parse::<u128>()
            .map_err(|_| value_invalid(path, "$int 不是有效十进制无符号整数"))?
    };
    let maximum = (1_u128 << bits) - 1;
    if parsed > maximum {
        return Err(value_invalid(
            path,
            format!("无符号整数超出 {bits}-bit 范围"),
        ));
    }
    u64::try_from(parsed).map_err(|_| value_invalid(path, "无符号整数超出 u64 范围"))
}

fn integer_tag<'a>(
    value: &'a Value,
    bits: u64,
    signed: bool,
    path: &str,
) -> Result<&'a str, JlinkError> {
    let object = require_object(value, path, "整数值必须是安全 JSON integer 或 $int object")?;
    if object.len() != 3 {
        return Err(value_invalid(
            path,
            "$int object 必须只包含 $int、bits、signed",
        ));
    }
    if object.get("bits").and_then(Value::as_u64) != Some(bits)
        || object.get("signed").and_then(Value::as_bool) != Some(signed)
    {
        return Err(value_invalid(
            path,
            "$int 的 bits 或 signed 与 DWARF 类型不一致",
        ));
    }
    object
        .get("$int")
        .and_then(Value::as_str)
        .ok_or_else(|| value_invalid(path, "$int 必须是十进制字符串"))
}

fn parse_pointer(value: &Value, byte_size: u64, path: &str) -> Result<u64, JlinkError> {
    let object = require_object(value, path, "指针值必须是 $pointer object")?;
    if object.len() != 1 {
        return Err(value_invalid(path, "$pointer object 不能包含其他字段"));
    }
    let text = object
        .get("$pointer")
        .and_then(Value::as_str)
        .ok_or_else(|| value_invalid(path, "$pointer 必须是 0x 十六进制字符串"))?;
    let digits = text
        .strip_prefix("0x")
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| value_invalid(path, "$pointer 必须是 0x 十六进制字符串"))?;
    let address = u64::from_str_radix(digits, 16)
        .map_err(|_| value_invalid(path, "$pointer 超出 u64 范围"))?;
    let bits = byte_size
        .checked_mul(8)
        .ok_or_else(|| type_unsupported(path, "指针位宽溢出"))?;
    if bits == 0 || bits > 64 || (bits < 64 && address > bit_mask(bits, path)?) {
        return Err(value_invalid(path, "指针值超出目标指针宽度"));
    }
    Ok(address)
}

fn member_bounds(
    member: &AccessMember,
    aggregate_len: usize,
    path: &str,
) -> Result<(usize, usize, Option<BitRange>), JlinkError> {
    let (size, bit_range) = match (
        member.storage_size(),
        member.dwarf_bit_offset(),
        member.bit_size(),
    ) {
        (None, None, None) => (bounded_layout_size(member.layout(), path)?, None),
        (Some(storage_size), Some(dwarf_offset), Some(width)) => {
            let storage_bits = storage_size
                .checked_mul(8)
                .ok_or_else(|| type_unsupported(path, "位域 storage size 溢出"))?;
            let lsb = storage_bits
                .checked_sub(dwarf_offset)
                .and_then(|value| value.checked_sub(width))
                .ok_or_else(|| type_unsupported(path, "DWARF v4 位域 offset 无效"))?;
            (storage_size, Some(BitRange::new(lsb, width)))
        }
        _ => return Err(type_unsupported(path, "位域元数据不完整")),
    };
    let start = to_usize(member.byte_offset(), path)?;
    let size = to_usize(size, path)?;
    let end = start
        .checked_add(size)
        .ok_or_else(|| type_unsupported(path, "成员范围溢出"))?;
    if end > aggregate_len {
        return Err(type_unsupported(path, "成员范围超出 aggregate 存储"));
    }
    Ok((start, end, bit_range))
}

fn validate_member_set(
    object: &Map<String, Value>,
    members: &[AccessMember],
    path: &str,
) -> Result<(), JlinkError> {
    if object.len() != members.len() {
        return Err(value_invalid(
            path,
            "结构体写入必须提供完整且唯一的成员集合",
        ));
    }
    if let Some(extra) = object
        .keys()
        .find(|name| !members.iter().any(|member| member.name() == name.as_str()))
    {
        return Err(value_invalid(
            &member_path(path, extra),
            "结构体包含未知成员",
        ));
    }
    if let Some(missing) = members
        .iter()
        .find(|member| !object.contains_key(member.name()))
    {
        return Err(value_invalid(
            &member_path(path, missing.name()),
            "结构体缺少成员",
        ));
    }
    Ok(())
}

fn bounded_layout_size(layout: &AccessLayout, path: &str) -> Result<u64, JlinkError> {
    layout
        .byte_size()
        .ok_or_else(|| type_unsupported(path, "值布局没有固定字节长度"))
}

fn require_length(actual: usize, expected: u64, path: &str) -> Result<(), JlinkError> {
    if u64::try_from(actual).ok() == Some(expected) {
        Ok(())
    } else {
        Err(value_invalid(
            path,
            format!("存储需要 {expected} 字节，实际为 {actual} 字节"),
        ))
    }
}

fn require_object<'a>(
    value: &'a Value,
    path: &str,
    message: &str,
) -> Result<&'a Map<String, Value>, JlinkError> {
    value
        .as_object()
        .ok_or_else(|| value_invalid(path, message))
}

fn reject_bit_range(bit_range: Option<BitRange>, path: &str) -> Result<(), JlinkError> {
    if bit_range.is_some() {
        Err(type_unsupported(path, "位域只能应用于标量布局"))
    } else {
        Ok(())
    }
}

fn read_unsigned(data: &[u8], path: &str) -> Result<u64, JlinkError> {
    if data.is_empty() || data.len() > 8 {
        return Err(type_unsupported(path, "V1 标量存储必须为 1..8 字节"));
    }
    let mut bytes = [0_u8; 8];
    bytes[..data.len()].copy_from_slice(data);
    Ok(u64::from_le_bytes(bytes))
}

fn write_unsigned(data: &mut [u8], value: u64, path: &str) -> Result<(), JlinkError> {
    if data.is_empty() || data.len() > 8 {
        return Err(type_unsupported(path, "V1 标量存储必须为 1..8 字节"));
    }
    let bits = u64::try_from(data.len())
        .ok()
        .and_then(|length| length.checked_mul(8))
        .ok_or_else(|| type_unsupported(path, "存储位宽溢出"))?;
    if bits < 64 && value > bit_mask(bits, path)? {
        return Err(value_invalid(path, "整数值超出存储宽度"));
    }
    data.copy_from_slice(&value.to_le_bytes()[..data.len()]);
    Ok(())
}

fn bit_mask(bits: u64, path: &str) -> Result<u64, JlinkError> {
    match bits {
        1..=63 => Ok((1_u64 << bits) - 1),
        64 => Ok(u64::MAX),
        _ => Err(type_unsupported(path, "逻辑整数宽度必须为 1..64 bit")),
    }
}

fn sign_extend(raw: u64, bits: u64, path: &str) -> Result<i64, JlinkError> {
    let mask = bit_mask(bits, path)?;
    if bits == 64 {
        Ok(raw.cast_signed())
    } else {
        let sign = 1_u64 << (bits - 1);
        Ok(if raw & sign == 0 {
            raw.cast_signed()
        } else {
            (raw | !mask).cast_signed()
        })
    }
}

fn signed_to_raw(value: i128, bits: u64, path: &str) -> Result<u64, JlinkError> {
    let raw = if value < 0 {
        (1_i128 << bits) + value
    } else {
        value
    };
    u64::try_from(raw).map_err(|_| value_invalid(path, "有符号整数无法编码到目标宽度"))
}

fn unsigned_json(value: u64, bits: u64) -> Value {
    if value <= JSON_SAFE_INTEGER {
        Value::Number(Number::from(value))
    } else {
        json!({ "$int": value.to_string(), "bits": bits, "signed": false })
    }
}

fn signed_json(value: i64, bits: u64) -> Value {
    if value.unsigned_abs() <= JSON_SAFE_INTEGER {
        Value::Number(Number::from(value))
    } else {
        json!({ "$int": value.to_string(), "bits": bits, "signed": true })
    }
}

fn to_usize(value: u64, path: &str) -> Result<usize, JlinkError> {
    usize::try_from(value).map_err(|_| type_unsupported(path, "长度超出平台 usize 范围"))
}

fn member_path(path: &str, member: &str) -> String {
    format!("{path}.{member}")
}

fn union_member_path(path: &str, member: &str) -> String {
    format!("{path}.$union.{member}")
}

fn index_path(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn value_invalid(path: &str, message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false).with_detail("path", json!(path))
}

fn type_unsupported(path: &str, message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TypeUnsupported, message, false).with_detail("path", json!(path))
}
