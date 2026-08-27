use std::collections::{BTreeMap, BTreeSet};

use jlink_domain::{
    AccessLayout, ErrorCode, HssRunState, HssThresholdRule, HssVariablePlan, JlinkError,
    ScalarEncoding, VariableSelector, decode_typed_value, normalize_hss_rules,
    normalize_hss_timestamp_us,
};
use serde::Serialize;
use serde_json::Value;

use crate::CaptureSnapshot;

/// Validated request for one deterministic changes projection.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureChangesQuery {
    series: Option<Vec<String>>,
    from_us: u64,
    to_us: u64,
    rules: Option<Vec<HssThresholdRule>>,
    limit: usize,
    offset: usize,
}

impl CaptureChangesQuery {
    /// Validates optional leaf selection, time range, rule override, and row limit.
    ///
    /// An explicit rule list replaces the capture's start-time rules for this
    /// query. Omitting it reuses the persisted start-time rules.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] for empty/duplicate series, invalid
    /// paths, invalid rules, an empty time range, or a limit outside 1..1000.
    pub fn new(
        series: Option<Vec<String>>,
        from_us: Option<u64>,
        to_us: Option<u64>,
        rules: Option<Vec<HssThresholdRule>>,
        limit: usize,
    ) -> Result<Self, JlinkError> {
        if !(1..=1_000).contains(&limit) {
            return Err(query_value_invalid("changes.limit 必须为 1..1000"));
        }
        let series = series.map(normalize_series_selection).transpose()?;
        let from_us = from_us.unwrap_or(0);
        let to_us = to_us.unwrap_or(u64::MAX);
        if from_us >= to_us {
            return Err(query_value_invalid(
                "changes 时间范围必须满足 from_us < to_us",
            ));
        }
        Ok(Self {
            series,
            from_us,
            to_us,
            rules: rules.map(normalize_hss_rules).transpose()?,
            limit,
            offset: 0,
        })
    }

    /// Sets the deterministic combined change/match row offset for cursor continuation.
    #[must_use]
    pub const fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }
}

/// One exact adjacent-value change observed between two source samples.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureChange {
    /// Stable short leaf-series ID.
    pub series: String,
    /// Latest source time at which the prior value was observed.
    pub after_us: u64,
    /// First source time at which the new value was observed.
    pub observed_by_us: u64,
    /// Complete prior `TypedValue` for this leaf.
    pub from: Value,
    /// Complete new `TypedValue` for this leaf.
    pub to: Value,
}

/// One declared threshold match kept distinct from exact changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRuleMatch {
    /// Stable normalized rule ID.
    pub rule: String,
    /// Stable short leaf-series ID.
    pub series: String,
    /// Latest source time at which the prior value was observed.
    pub after_us: u64,
    /// First source time at which the match was observed.
    pub observed_by_us: u64,
}

/// Bounded deterministic changes result over one immutable capture snapshot.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureChanges {
    /// First-use mapping for every series referenced by this page.
    pub dictionary: BTreeMap<String, String>,
    /// Exact changes in source-record and stable leaf order.
    pub changes: Vec<CaptureChange>,
    /// Threshold matches in source-record, rule-ID, and stable leaf order.
    pub matches: Vec<CaptureRuleMatch>,
    /// Whether more deterministic rows exist after this bounded page.
    pub truncated: bool,
}

/// Projects exact changes and declarative threshold matches from one immutable capture.
///
/// # Errors
///
/// Returns a stable state, frame, path, type, or value error when the snapshot,
/// requested leaves, wildcard rules, or typed comparisons cannot be evaluated
/// without ambiguity or precision loss.
pub fn changes(
    snapshot: &CaptureSnapshot,
    query: &CaptureChangesQuery,
) -> Result<CaptureChanges, JlinkError> {
    if snapshot.status().state != HssRunState::Completed {
        return Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "changes 只接受生命周期为 completed 的不可变 capture；请先查询 status",
            false,
        ));
    }
    let payload = snapshot.read_verified_payload()?;
    let batch = snapshot.plan().frame_layout().parse(&payload)?;
    validate_complete_frames(snapshot, &batch)?;
    let catalog = series_catalog(snapshot)?;
    let selected = resolve_series(&catalog, query.series.as_deref())?;
    let effective_rules = query
        .rules
        .as_deref()
        .unwrap_or_else(|| snapshot.plan().rules());
    let rule_bindings = bind_rules(&catalog, effective_rules)?;
    let rows = collect_rows(
        snapshot,
        &batch.frames,
        &catalog,
        &selected,
        &rule_bindings,
        query,
    )?;
    Ok(rows.into_result(&catalog))
}

#[derive(Clone, Debug)]
pub(crate) struct SeriesDescriptor {
    pub(crate) id: String,
    pub(crate) path: String,
    top_level: usize,
    value_steps: Vec<ValueStep>,
    pub(crate) numeric: bool,
}

#[derive(Clone, Debug)]
enum ValueStep {
    Member(String),
    Index(usize),
}

#[derive(Clone, Debug)]
struct LeafDescriptor {
    path: String,
    value_steps: Vec<ValueStep>,
    numeric: bool,
}

pub(crate) fn series_catalog(
    snapshot: &CaptureSnapshot,
) -> Result<Vec<SeriesDescriptor>, JlinkError> {
    let mut catalog = Vec::new();
    for (top_level, variable) in snapshot.plan().variables().iter().enumerate() {
        let base = variable.access_plan().selector().path();
        let mut leaves = Vec::new();
        append_layout_leaves(
            variable.access_plan().layout(),
            base,
            &mut Vec::new(),
            variable
                .access_plan()
                .selector()
                .slice()
                .map(jlink_domain::ElementSlice::start),
            &mut leaves,
        )?;
        for (ordinal, leaf) in leaves.into_iter().enumerate() {
            let id = if leaf.value_steps.is_empty() {
                format!("s{top_level}")
            } else {
                format!("s{top_level}.{ordinal}")
            };
            catalog.push(SeriesDescriptor {
                id,
                path: leaf.path,
                top_level,
                value_steps: leaf.value_steps,
                numeric: leaf.numeric,
            });
        }
    }
    Ok(catalog)
}

fn append_layout_leaves(
    layout: &AccessLayout,
    path: &str,
    value_steps: &mut Vec<ValueStep>,
    root_slice_start: Option<u64>,
    leaves: &mut Vec<LeafDescriptor>,
) -> Result<(), JlinkError> {
    match layout {
        AccessLayout::Structure { members, .. } => {
            for member in members {
                value_steps.push(ValueStep::Member(member.name().to_owned()));
                append_layout_leaves(
                    member.layout(),
                    &format!("{path}.{}", member.name()),
                    value_steps,
                    None,
                    leaves,
                )?;
                value_steps.pop();
            }
        }
        AccessLayout::Array {
            element,
            count: Some(count),
        } => {
            let start = root_slice_start.unwrap_or(0);
            for local_index in 0..*count {
                let local = usize::try_from(local_index)
                    .map_err(|_| query_frame_invalid("数组索引无法表示为 usize"))?;
                let actual = start
                    .checked_add(local_index)
                    .ok_or_else(|| query_frame_invalid("slice 数组索引溢出"))?;
                value_steps.push(ValueStep::Index(local));
                append_layout_leaves(
                    element,
                    &format!("{path}[{actual}]"),
                    value_steps,
                    None,
                    leaves,
                )?;
                value_steps.pop();
            }
        }
        AccessLayout::Array { count: None, .. } => {
            return Err(query_frame_invalid(
                "不可变 capture 包含未通过 slice 收敛的无界数组",
            ));
        }
        AccessLayout::Scalar { encoding, .. } => leaves.push(LeafDescriptor {
            path: path.to_owned(),
            value_steps: value_steps.clone(),
            numeric: matches!(
                encoding,
                ScalarEncoding::Signed | ScalarEncoding::Unsigned | ScalarEncoding::Float
            ),
        }),
        AccessLayout::Pointer { .. } | AccessLayout::Union { .. } => {
            leaves.push(LeafDescriptor {
                path: path.to_owned(),
                value_steps: value_steps.clone(),
                numeric: false,
            });
        }
    }
    Ok(())
}

pub(crate) fn normalize_series_selection(series: Vec<String>) -> Result<Vec<String>, JlinkError> {
    if series.is_empty() {
        return Err(query_value_invalid("changes.series 不能为空数组"));
    }
    let mut normalized = Vec::with_capacity(series.len());
    let mut seen = BTreeSet::new();
    for item in series {
        let item = if item.starts_with('s')
            && item[1..]
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            item
        } else {
            VariableSelector::new(&item, None)?.path().to_owned()
        };
        if !seen.insert(item.clone()) {
            return Err(query_value_invalid("changes.series 不能包含重复项")
                .with_detail("series", serde_json::json!(item)));
        }
        normalized.push(item);
    }
    Ok(normalized)
}

pub(crate) fn resolve_series(
    catalog: &[SeriesDescriptor],
    requested: Option<&[String]>,
) -> Result<Vec<usize>, JlinkError> {
    let Some(requested) = requested else {
        return Ok((0..catalog.len()).collect());
    };
    let mut selected = BTreeSet::new();
    for request in requested {
        let is_top_level_id = request.strip_prefix('s').is_some_and(|index| {
            !index.is_empty()
                && !index.contains('.')
                && index.bytes().all(|byte| byte.is_ascii_digit())
        });
        let matches = catalog
            .iter()
            .enumerate()
            .filter(|(_, descriptor)| {
                descriptor.id == *request
                    || descriptor.path == *request
                    || (is_top_level_id
                        && descriptor
                            .id
                            .strip_prefix(request.as_str())
                            .is_some_and(|suffix| suffix.starts_with('.')))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(query_value_invalid("changes.series 未匹配已采集叶路径")
                .with_detail("series", serde_json::json!(request)));
        }
        if matches.len() > 1 && !is_top_level_id {
            return Err(query_value_invalid(
                "changes.series 路径对应多个采集序列，请使用短 series ID",
            )
            .with_detail("series", serde_json::json!(request)));
        }
        selected.extend(matches);
    }
    Ok(selected.into_iter().collect())
}

#[derive(Clone, Debug)]
struct RuleBinding<'a> {
    rule: &'a HssThresholdRule,
    series: Vec<usize>,
}

fn bind_rules<'a>(
    catalog: &[SeriesDescriptor],
    rules: &'a [HssThresholdRule],
) -> Result<Vec<RuleBinding<'a>>, JlinkError> {
    let mut bindings = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut series = Vec::new();
        for (index, descriptor) in catalog.iter().enumerate() {
            if rule.matches_path(&descriptor.path)? {
                series.push(index);
            }
        }
        if series.is_empty() {
            return Err(query_value_invalid("HSS 规则 path 未匹配已采集叶路径")
                .with_detail("rule_id", serde_json::json!(rule.id()))
                .with_detail("path", serde_json::json!(rule.path())));
        }
        if !matches!(rule, HssThresholdRule::Equals { .. })
            && series.iter().any(|index| !catalog[*index].numeric)
        {
            return Err(query_value_invalid("数值 HSS 规则只能应用于数值叶路径")
                .with_detail("rule_id", serde_json::json!(rule.id())));
        }
        bindings.push(RuleBinding { rule, series });
    }
    Ok(bindings)
}

#[derive(Clone, Debug)]
enum Row {
    Change(CaptureChange),
    Match(CaptureRuleMatch),
}

#[derive(Clone, Debug)]
struct CollectedRows {
    rows: Vec<Row>,
    has_more: bool,
    skipped: usize,
    offset: usize,
}

impl CollectedRows {
    const fn new(offset: usize) -> Self {
        Self {
            rows: Vec::new(),
            has_more: false,
            skipped: 0,
            offset,
        }
    }

    fn push(&mut self, row: Row, limit: usize) -> bool {
        if self.skipped < self.offset {
            self.skipped += 1;
            return true;
        }
        if self.rows.len() == limit {
            self.has_more = true;
            return false;
        }
        self.rows.push(row);
        true
    }

    fn into_result(self, catalog: &[SeriesDescriptor]) -> CaptureChanges {
        let mut dictionary = BTreeMap::new();
        let mut changes = Vec::new();
        let mut matches = Vec::new();
        for row in self.rows {
            let series = match &row {
                Row::Change(change) => &change.series,
                Row::Match(rule_match) => &rule_match.series,
            };
            if let Some(descriptor) = catalog.iter().find(|item| item.id == *series) {
                dictionary.insert(series.clone(), descriptor.path.clone());
            }
            match row {
                Row::Change(change) => changes.push(change),
                Row::Match(rule_match) => matches.push(rule_match),
            }
        }
        CaptureChanges {
            dictionary,
            changes,
            matches,
            truncated: self.has_more,
        }
    }
}

fn collect_rows(
    snapshot: &CaptureSnapshot,
    frames: &[jlink_domain::HssRawFrame<'_>],
    catalog: &[SeriesDescriptor],
    selected: &[usize],
    rules: &[RuleBinding<'_>],
    query: &CaptureChangesQuery,
) -> Result<CollectedRows, JlinkError> {
    let Some(first) = frames.first() else {
        return Ok(CollectedRows::new(query.offset));
    };
    let mut rows = CollectedRows::new(query.offset);
    let mut before = decode_frame(snapshot.plan().variables(), first.sample)?;
    let mut previous_timestamp_raw = first.timestamp_raw;
    for frame in &frames[1..] {
        let after_us = normalize_hss_timestamp_us(previous_timestamp_raw);
        let observed_by_us = normalize_hss_timestamp_us(frame.timestamp_raw);
        let after = decode_frame(snapshot.plan().variables(), frame.sample)?;
        if observed_by_us < after_us {
            return Err(query_frame_invalid(
                "源时间戳回退，changes 无法声明确定的相邻观测区间",
            ));
        }
        if observed_by_us >= query.from_us && observed_by_us < query.to_us {
            let context = PairRowsContext {
                catalog,
                selected,
                rules,
                after_us,
                observed_by_us,
                limit: query.limit,
            };
            if !append_pair_rows(&mut rows, &before, &after, &context)? {
                break;
            }
        }
        before = after;
        previous_timestamp_raw = frame.timestamp_raw;
    }
    Ok(rows)
}

struct PairRowsContext<'a> {
    catalog: &'a [SeriesDescriptor],
    selected: &'a [usize],
    rules: &'a [RuleBinding<'a>],
    after_us: u64,
    observed_by_us: u64,
    limit: usize,
}

fn append_pair_rows(
    rows: &mut CollectedRows,
    before: &[Value],
    after: &[Value],
    context: &PairRowsContext<'_>,
) -> Result<bool, JlinkError> {
    for index in context.selected {
        let descriptor = &context.catalog[*index];
        let from = descriptor.value(before)?.clone();
        let to = descriptor.value(after)?.clone();
        if from != to
            && !rows.push(
                Row::Change(CaptureChange {
                    series: descriptor.id.clone(),
                    after_us: context.after_us,
                    observed_by_us: context.observed_by_us,
                    from,
                    to,
                }),
                context.limit,
            )
        {
            return Ok(false);
        }
    }
    for binding in context.rules {
        for index in &binding.series {
            let descriptor = &context.catalog[*index];
            if binding
                .rule
                .matches_values(descriptor.value(before)?, descriptor.value(after)?)?
                && !rows.push(
                    Row::Match(CaptureRuleMatch {
                        rule: binding.rule.id().to_owned(),
                        series: descriptor.id.clone(),
                        after_us: context.after_us,
                        observed_by_us: context.observed_by_us,
                    }),
                    context.limit,
                )
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

impl SeriesDescriptor {
    pub(crate) fn value<'a>(&self, decoded: &'a [Value]) -> Result<&'a Value, JlinkError> {
        let mut value = decoded
            .get(self.top_level)
            .ok_or_else(|| query_frame_invalid("HSS 顶层解码值缺失"))?;
        for step in &self.value_steps {
            value = match step {
                ValueStep::Member(member) => value
                    .get(member)
                    .ok_or_else(|| query_frame_invalid("HSS 结构成员解码值缺失"))?,
                ValueStep::Index(index) => value
                    .get(*index)
                    .ok_or_else(|| query_frame_invalid("HSS 数组元素解码值缺失"))?,
            };
        }
        Ok(value)
    }
}

pub(crate) fn decode_frame(
    variables: &[HssVariablePlan],
    sample: &[u8],
) -> Result<Vec<Value>, JlinkError> {
    variables
        .iter()
        .map(|variable| {
            let start = usize::try_from(variable.sample_offset())
                .map_err(|_| query_frame_invalid("HSS sample_offset 无法表示为 usize"))?;
            let size = usize::try_from(variable.access_plan().byte_size())
                .map_err(|_| query_frame_invalid("HSS byte_size 无法表示为 usize"))?;
            let end = start
                .checked_add(size)
                .ok_or_else(|| query_frame_invalid("HSS 样本变量范围溢出"))?;
            decode_typed_value(
                variable.access_plan(),
                sample
                    .get(start..end)
                    .ok_or_else(|| query_frame_invalid("HSS 样本变量范围越界"))?,
            )
        })
        .collect()
}

pub(crate) fn validate_complete_frames(
    snapshot: &CaptureSnapshot,
    batch: &jlink_domain::HssFrameBatch<'_>,
) -> Result<(), JlinkError> {
    if !batch.incomplete_tail.is_empty() {
        return Err(query_frame_invalid("完成 capture 包含非完整 HSS 记录尾部"));
    }
    let samples = u64::try_from(batch.frames.len())
        .map_err(|_| query_frame_invalid("完成 capture 样本数量无法表示为 u64"))?;
    if samples != snapshot.status().complete_records
        || samples != snapshot.status().quality.actual_samples
    {
        return Err(query_frame_invalid(
            "完成 capture 的样本计数与终态清单不一致",
        ));
    }
    Ok(())
}

fn query_value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}

fn query_frame_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::FrameInvalid, message, false)
}
