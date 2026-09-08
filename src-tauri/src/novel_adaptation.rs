use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::{self, DbState};
use crate::novel::{
    canonicalize_comic_plan_intent, comic_plan_intent_constraints, comic_plan_profile_instruction,
    ensure_active_work, get_work, json_value, new_id, now, request_hash, with_receipt,
    ComicPlanDialogueIntent, ComicPlanIntent,
};
use crate::AppState;

const TOKEN_TTL_MS: i64 = 15 * 60 * 1000;
// The configured request timeout is five minutes.  Keep the lease slightly longer
// so a legitimate model response can still be committed by its owner.
const ADAPTATION_ANALYSIS_LEASE_MS: i64 = 360_000;
const ADAPTATION_ANALYSIS_SCHEMA: &str =
    include_str!("../../src/shared/contracts/novel-analysis.v1.schema.json");
const ORIGINAL_ARTIFACT_TYPES: [&str; 10] = [
    "chapter_summary",
    "chapter_beats",
    "world_facts",
    "character_facts",
    "faction_facts",
    "location_facts",
    "prop_facts",
    "timeline_delta",
    "continuity_delta",
    "open_threads",
];
const ADAPTATION_ARTIFACT_TYPES: [&str; 4] = [
    "adaptation_proposal",
    "comic_chapter_plan",
    "scene_plan",
    "page_panel_plan",
];
const COMIC_PRODUCTION_CONTRACT: &str =
    include_str!("../../src/shared/comic-production-contract.json");
const COMIC_PAGE_LAYOUT_EXAMPLES: &str =
    include_str!("../../src/shared/comic-page-layout-examples.json");
const ADAPTATION_ANALYSIS_PROMPT_VERSION_V1: &str = "adaptation-analysis.v1";
const ADAPTATION_ANALYSIS_PROMPT_VERSION_V2: &str = "adaptation-analysis.v2";
const ADAPTATION_ANALYSIS_PROMPT_VERSION_V3: &str = "adaptation-analysis.v3";
const ADAPTATION_ANALYSIS_PROMPT_VERSION_V4: &str = "adaptation-analysis.v4";
const ADAPTATION_ANALYSIS_PROMPT_VERSION: &str = "adaptation-analysis.v5";
type PlanningSourceRange = (String, i64, i64);
type PlanningChapterRanges = (String, Vec<PlanningSourceRange>);
type FrozenSourceInputs = (String, Option<String>, Vec<(String, String, String)>);
type SnapshotRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn layout_examples_for_frozen_constraints(
    constraints: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let selected = constraints
        .and_then(|value| value.get("pages"))
        .and_then(Value::as_array)
        .map(|pages| {
            pages
                .iter()
                .filter_map(|page| page.get("layoutProfile").and_then(Value::as_str))
                .filter(|profile| matches!(*profile, "reference_story_5" | "hero_middle_5"))
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let profiles = serde_json::from_str::<Value>(COMIC_PAGE_LAYOUT_EXAMPLES)
        .map_err(|_| "COMIC_LAYOUT_EXAMPLES_INVALID".to_string())?
        .get("profiles")
        .and_then(Value::as_object)
        .cloned()
        .ok_or("COMIC_LAYOUT_EXAMPLES_INVALID")?;
    selected
        .into_iter()
        .map(|profile| {
            profiles
                .get(profile)
                .cloned()
                .ok_or_else(|| "COMIC_LAYOUT_EXAMPLES_INVALID".to_string())
        })
        .collect()
}

#[cfg(test)]
fn adaptation_analysis_system_prompt_for_version(version: &str) -> Result<String, String> {
    adaptation_analysis_system_prompt_for_frozen_constraints(version, None)
}

fn adaptation_analysis_system_prompt_for_frozen_constraints(
    version: &str,
    frozen_constraints: Option<&Value>,
) -> Result<String, String> {
    let common = "你是改编规划分析器。只输出聚合 JSON {\"artifacts\":[...4 items...]}，没有 Markdown 或解释。artifacts 必须恰有 adaptation_proposal、comic_chapter_plan、scene_plan、page_panel_plan 各一项，绝不能输出十类原著分析。每项必须包含 schemaVersion='novel-analysis.v1'、artifactType、owner、content、warnings；ownerMap 是强制精确值。每项 content 必须符合下面冻结的 Novel Analysis v1 Schema 相应 oneOf，page_panel_plan 还必须有可验证的完整 geometry。";
    match version {
        ADAPTATION_ANALYSIS_PROMPT_VERSION_V1 => {
            Ok(format!("{common}\n{ADAPTATION_ANALYSIS_SCHEMA}"))
        }
        ADAPTATION_ANALYSIS_PROMPT_VERSION_V2 => Ok(format!(
            "{common}\n页面 geometry 必须同时满足下列正式页面生产合同和其数值阈值：每个 polygon 必须在 normalized-0-1 内、3 到 maxPolygonVertices 个点、严格凸，面积、包围宽度和高度均不得小于合同最小值；非嵌套 panel 不重叠且距离不少于 gutter。bounds 必须精确等于 polygon 的包围盒。safeArea 必须完整位于从页边内缩 minSafeInset 的矩形内；textZone 的宽高必须达到合同最小值，并同时完整位于 safeArea 和所属 polygon 内。bleed 标志必须与 polygon 实际触及的页边完全一致。panelCount、readingOrder、dominantPanel、templateId 与 geometry 必须相互一致。允许嵌套时 parentPanelNo 必须存在、不得自身或循环、子 polygon 必须完整位于父 polygon 内。\n若 user JSON 给出 comicPlanIntentConstraints，必须逐字段满足其中页数、格数、版式数值和拓扑关系、以及对白要求；它是目标约束而不是可复制的现成 polygon geometry，必须输出新的完整实际 geometry。\n正式页面生产合同：\n{COMIC_PRODUCTION_CONTRACT}\n冻结 Novel Analysis v1 Schema：\n{ADAPTATION_ANALYSIS_SCHEMA}"
        )),
        ADAPTATION_ANALYSIS_PROMPT_VERSION_V3 => {
            let examples = layout_examples_for_frozen_constraints(frozen_constraints)?;
            let example_instruction = if examples.is_empty() {
                String::new()
            } else {
                format!(
                    "\n若 comicPlanIntentConstraints 选择下列受控 profile，以下是对应的几何模板片段（不是完整 artifact、不是剧情/对白/owner）。constraints 本身不是 polygon geometry；但命中这些受控 profile 时，允许且建议直接复用对应示例的完整 geometry 坐标作为该页 layout.geometry。仍必须按 schema 生成完整 layout 和剧情字段、source/mappings 与对白，并通过全部合同、intent 和对白约束：\n{}",
                    Value::Array(examples)
                )
            };
            Ok(format!(
                "{common}\n页面 geometry 必须同时满足下列正式页面生产合同和其数值阈值：每个 polygon 必须在 normalized-0-1 内、3 到 maxPolygonVertices 个点、严格凸，面积、包围宽度和高度均不得小于合同最小值；非嵌套 panel 不重叠且距离不少于 gutter。bounds 必须精确等于 polygon 的包围盒。safeArea 必须完整位于从页边内缩 minSafeInset 的矩形内；textZone 的宽高必须达到合同最小值，并同时完整位于 safeArea 和所属 polygon 内。bleed 标志必须与 polygon 实际触及的页边完全一致。panelCount、readingOrder、dominantPanel、templateId 与 geometry 必须相互一致。允许嵌套时 parentPanelNo 必须存在、不得自身或循环、子 polygon 必须完整位于父 polygon 内。\n若 user JSON 给出 comicPlanIntentConstraints，必须逐字段满足其中页数、格数、版式数值和拓扑关系、以及对白要求。没有匹配的受控示例时，必须自行生成合法的完整实际 geometry。{example_instruction}\n正式页面生产合同：\n{COMIC_PRODUCTION_CONTRACT}\n冻结 Novel Analysis v1 Schema：\n{ADAPTATION_ANALYSIS_SCHEMA}"
            ))
        }
        ADAPTATION_ANALYSIS_PROMPT_VERSION_V4 => Ok(format!(
            "{}\nuser JSON 中的 sourceRangeOptions 是经本 run 冻结并核实的 UTF-8 字节坐标片段，不是完整 evidence_ref。每个 comic_chapter_plan.sourceSelections 必须从某个 option 逐字复制 novelChapterRevisionId、startUtf8Byte、endUtf8Byte，禁止自行计算、拆分、取整或改写字节 offset。option 的 verifiedExcerpt 是该 immutable revision 的真实原文短片段；按正式 schema 输出时仍须填写 evidenceExcerpt（可引用相应 verifiedExcerpt 的真实文字）和 0..1 confidence，不能凭空编造证据。",
            adaptation_analysis_system_prompt_for_frozen_constraints(
                ADAPTATION_ANALYSIS_PROMPT_VERSION_V3,
                frozen_constraints,
            )?
        )),
        ADAPTATION_ANALYSIS_PROMPT_VERSION => Ok(format!(
            "{}\nv5 user JSON 的 outputIdentityBindings 是本 run 已冻结的精确身份映射，必须逐字遵守：adaptation_proposal 的 envelope owner 必须等于 outputIdentityBindings.adaptation_proposal.owner；comic_chapter_plan、scene_plan、page_panel_plan 的 envelope owner 必须分别等于对应 binding 的 owner。scene_plan.content.comicChapterDraftId 和 page_panel_plan.content.comicChapterDraftId 必须逐字等于各自 binding 的 content.comicChapterDraftId（即 ownerMap.comic_chapter 的物理 comic_adaptation_chapter ID）。它绝不是 comic_chapter_plan.chapters[].stableKey、任何 source revision/run ID、page stableKey、scene stableKey 或 panel stableKey。comic_chapter_plan.chapters[].stableKey 仅是下游 planning chapter 的 stable key；若 page panel 提供 planningSceneStableKey，它必须引用本次 scene_plan.scenes[].stableKey 之一。",
            adaptation_analysis_system_prompt_for_frozen_constraints(
                ADAPTATION_ANALYSIS_PROMPT_VERSION_V4,
                frozen_constraints,
            )?
        )),
        _ => Err("FROZEN_PROMPT_VERSION_UNKNOWN".into()),
    }
}

#[cfg(test)]
fn adaptation_analysis_system_prompt_hash_for_version(version: &str) -> Result<String, String> {
    adaptation_analysis_system_prompt_hash_for_frozen_constraints(version, None)
}

fn adaptation_analysis_system_prompt_hash_for_frozen_constraints(
    version: &str,
    frozen_constraints: Option<&Value>,
) -> Result<String, String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(
            adaptation_analysis_system_prompt_for_frozen_constraints(version, frozen_constraints)?
                .as_bytes()
        )
    ))
}

fn adaptation_prompt_uses_source_range_options(version: &str) -> bool {
    matches!(
        version,
        ADAPTATION_ANALYSIS_PROMPT_VERSION_V4 | ADAPTATION_ANALYSIS_PROMPT_VERSION
    )
}

fn adaptation_output_identity_bindings(adaptation_id: &str, chapter_id: &str) -> Value {
    let adaptation_owner = json!({"ownerType":"comic_adaptation","ownerId":adaptation_id});
    let chapter_owner = json!({"ownerType":"comic_chapter","ownerId":chapter_id});
    json!({
        "adaptation_proposal": {"owner":adaptation_owner},
        "comic_chapter_plan": {"owner":chapter_owner},
        "scene_plan": {
            "owner":{"ownerType":"comic_chapter","ownerId":chapter_id},
            "content":{"comicChapterDraftId":chapter_id}
        },
        "page_panel_plan": {
            "owner":{"ownerType":"comic_chapter","ownerId":chapter_id},
            "content":{"comicChapterDraftId":chapter_id}
        }
    })
}

#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|v| v.is_finite())
}
fn bounds_contains(outer: &Value, inner: &Value) -> bool {
    let (Some(x), Some(y), Some(w), Some(h), Some(ix), Some(iy), Some(iw), Some(ih)) = (
        number(outer.get("x")),
        number(outer.get("y")),
        number(outer.get("width")),
        number(outer.get("height")),
        number(inner.get("x")),
        number(inner.get("y")),
        number(inner.get("width")),
        number(inner.get("height")),
    ) else {
        return false;
    };
    ix >= x - 1e-6
        && iy >= y - 1e-6
        && iw >= 0.0
        && ih >= 0.0
        && ix + iw <= x + w + 1e-6
        && iy + ih <= y + h + 1e-6
}
fn polygon_area(points: &[Point]) -> f64 {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let q = points[(i + 1) % points.len()];
            p.x * q.y - q.x * p.y
        })
        .sum::<f64>()
        .abs()
        / 2.0
}
fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x)
}
fn strictly_convex(points: &[Point]) -> bool {
    let mut sign = 0.0_f64;
    for i in 0..points.len() {
        let turn = cross(
            points[i],
            points[(i + 1) % points.len()],
            points[(i + 2) % points.len()],
        );
        if turn.abs() < 1e-8 {
            return false;
        }
        if sign == 0.0 {
            sign = turn.signum()
        } else if sign != turn.signum() {
            return false;
        }
    }
    true
}
fn point_in_polygon(point: Point, points: &[Point]) -> bool {
    let mut inside = false;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        if ((a.y > point.y) != (b.y > point.y))
            && (point.x < (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x)
        {
            inside = !inside
        }
    }
    inside
}

#[derive(Clone)]
struct GeometryPanel {
    panel_no: i64,
    polygon: Vec<Point>,
    parent_panel_no: Option<i64>,
    bleed_top: bool,
    bleed_right: bool,
    bleed_bottom: bool,
    bleed_left: bool,
}

fn point_on_segment(point: Point, a: Point, b: Point) -> bool {
    cross(a, b, point).abs() <= 1e-8
        && point.x >= a.x.min(b.x) - 1e-8
        && point.x <= a.x.max(b.x) + 1e-8
        && point.y >= a.y.min(b.y) - 1e-8
        && point.y <= a.y.max(b.y) + 1e-8
}
fn point_in_or_on_polygon(point: Point, polygon: &[Point]) -> bool {
    polygon
        .iter()
        .enumerate()
        .any(|(i, a)| point_on_segment(point, *a, polygon[(i + 1) % polygon.len()]))
        || point_in_polygon(point, polygon)
}
fn segments_intersect(a: Point, b: Point, c: Point, d: Point) -> bool {
    let ab_c = cross(a, b, c);
    let ab_d = cross(a, b, d);
    let cd_a = cross(c, d, a);
    let cd_b = cross(c, d, b);
    (ab_c.signum() != ab_d.signum() && cd_a.signum() != cd_b.signum())
        || point_on_segment(c, a, b)
        || point_on_segment(d, a, b)
        || point_on_segment(a, c, d)
        || point_on_segment(b, c, d)
}
fn polygons_overlap(first: &[Point], second: &[Point]) -> bool {
    first.iter().enumerate().any(|(i, a)| {
        second.iter().enumerate().any(|(j, b)| {
            segments_intersect(
                *a,
                first[(i + 1) % first.len()],
                *b,
                second[(j + 1) % second.len()],
            )
        })
    }) || first.iter().any(|p| point_in_polygon(*p, second))
        || second.iter().any(|p| point_in_polygon(*p, first))
}
fn point_segment_distance(point: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let denominator = dx * dx + dy * dy;
    let t = if denominator == 0.0 {
        0.0
    } else {
        (((point.x - a.x) * dx + (point.y - a.y) * dy) / denominator).clamp(0.0, 1.0)
    };
    let x = a.x + t * dx;
    let y = a.y + t * dy;
    ((point.x - x).powi(2) + (point.y - y).powi(2)).sqrt()
}
fn polygon_distance(first: &[Point], second: &[Point]) -> f64 {
    first
        .iter()
        .flat_map(|point| {
            second.iter().enumerate().map(move |(i, start)| {
                point_segment_distance(*point, *start, second[(i + 1) % second.len()])
            })
        })
        .chain(second.iter().flat_map(|point| {
            first.iter().enumerate().map(move |(i, start)| {
                point_segment_distance(*point, *start, first[(i + 1) % first.len()])
            })
        }))
        .fold(f64::INFINITY, f64::min)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutValidationFailure {
    reason: &'static str,
    panel_index: Option<usize>,
    field: &'static str,
}

impl LayoutValidationFailure {
    const fn layout(reason: &'static str, field: &'static str) -> Self {
        Self {
            reason,
            panel_index: None,
            field,
        }
    }

    const fn panel(reason: &'static str, panel_index: usize, field: &'static str) -> Self {
        Self {
            reason,
            panel_index: Some(panel_index),
            field,
        }
    }

    fn adaptation_diagnostic(self, page_index: usize) -> String {
        let path = match self.panel_index {
            Some(panel_index) => format!(
                "pages[{page_index}].layout.geometry.panels[{panel_index}].{}",
                self.field
            ),
            None => format!("pages[{page_index}].layout.{}", self.field),
        };
        format!("LAYOUT_INVALID:{}:{path}", self.reason)
    }
}

fn validate_page_layout_detail(
    layout: &Value,
    panels: &[Value],
) -> Result<(), LayoutValidationFailure> {
    let contract: Value = serde_json::from_str(COMIC_PRODUCTION_CONTRACT)
        .map_err(|_| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    if contract.get("schemaVersion").and_then(Value::as_i64) != Some(1) {
        return Err(LayoutValidationFailure::layout("CONTRACT", "contract"));
    }
    let panel_count = layout
        .get("panelCount")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or_else(|| LayoutValidationFailure::layout("PANEL_COUNT", "panelCount"))?
        as usize;
    let reading = layout
        .get("readingOrder")
        .and_then(Value::as_array)
        .ok_or_else(|| LayoutValidationFailure::layout("READING_ORDER", "readingOrder"))?;
    let expected = (1..=panel_count as i64).collect::<std::collections::BTreeSet<_>>();
    let actual = reading
        .iter()
        .filter_map(Value::as_i64)
        .collect::<std::collections::BTreeSet<_>>();
    if panels.len() != panel_count
        || reading.len() != panel_count
        || actual != expected
        || layout
            .get("dominantPanel")
            .and_then(Value::as_i64)
            .filter(|v| actual.contains(v))
            .is_none()
    {
        return Err(LayoutValidationFailure::layout(
            "PANEL_IDENTITY",
            "readingOrder",
        ));
    }
    let panel_nos = panels
        .iter()
        .filter_map(|panel| panel.get("panelNo").and_then(Value::as_i64))
        .collect::<std::collections::BTreeSet<_>>();
    if panel_nos != actual {
        return Err(LayoutValidationFailure::layout("PANEL_IDENTITY", "panels"));
    }
    let custom = layout.get("templateId").and_then(Value::as_str) == Some("custom_irregular")
        || layout.get("layoutKind").and_then(Value::as_str) == Some("custom_irregular");
    if !custom {
        let expected = match layout.get("templateId").and_then(Value::as_str) {
            Some(
                "reference_story_5" | "hero_middle_5" | "diagonal_action_5" | "detail_to_wide_5",
            ) => 5,
            Some("reveal_focus_4" | "full_bleed_insets_4") => 4,
            Some("conversation_ladder_6") => 6,
            Some("nine_grid_9") => 9,
            _ => return Err(LayoutValidationFailure::layout("TEMPLATE", "templateId")),
        };
        if panel_count != expected {
            return Err(LayoutValidationFailure::layout("TEMPLATE", "panelCount"));
        }
    }
    let geometry = layout
        .get("geometry")
        .ok_or_else(|| LayoutValidationFailure::layout("GEOMETRY", "geometry"))?;
    if geometry.get("coordinateSystem").and_then(Value::as_str) != Some("normalized-0-1")
        || geometry.get("panelCount").and_then(Value::as_u64) != Some(panel_count as u64)
        || geometry.get("readingOrder") != Some(&Value::Array(reading.clone()))
    {
        return Err(LayoutValidationFailure::layout("GEOMETRY", "geometry"));
    }
    let limits = &contract["comicPageGeometry"];
    let min_area = number(limits.get("minPanelArea"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_width = number(limits.get("minPanelWidth"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_height = number(limits.get("minPanelHeight"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_gutter = number(limits.get("minGutter"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_safe = number(limits.get("minSafeInset"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_text_w = number(limits.get("minTextZoneWidth"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    let min_text_h = number(limits.get("minTextZoneHeight"))
        .ok_or_else(|| LayoutValidationFailure::layout("CONTRACT", "contract"))?;
    if number(geometry.get("gutter"))
        .filter(|v| *v >= min_gutter)
        .is_none()
    {
        return Err(LayoutValidationFailure::layout("GUTTER", "geometry.gutter"));
    };
    let safe = geometry
        .get("safeArea")
        .ok_or_else(|| LayoutValidationFailure::layout("SAFE_AREA", "geometry.safeArea"))?;
    if !bounds_contains(
        &json!({"x":min_safe,"y":min_safe,"width":1.0-2.0*min_safe,"height":1.0-2.0*min_safe}),
        safe,
    ) {
        return Err(LayoutValidationFailure::layout(
            "SAFE_AREA",
            "geometry.safeArea",
        ));
    }
    let geometries = geometry
        .get("panels")
        .and_then(Value::as_array)
        .filter(|v| v.len() == panel_count)
        .ok_or_else(|| LayoutValidationFailure::layout("PANEL_COUNT", "geometry.panels"))?;
    let mut validated = Vec::with_capacity(panel_count);
    for (panel_index, raw) in geometries.iter().enumerate() {
        let no = raw
            .get("panelNo")
            .and_then(Value::as_i64)
            .filter(|v| actual.contains(v))
            .ok_or_else(|| {
                LayoutValidationFailure::panel("PANEL_IDENTITY", panel_index, "panelNo")
            })?;
        let points = raw
            .get("polygon")
            .and_then(Value::as_array)
            .filter(|v| v.len() >= 3 && v.len() <= 8)
            .ok_or_else(|| LayoutValidationFailure::panel("POLYGON", panel_index, "polygon"))?
            .iter()
            .map(|p| {
                let point = Point {
                    x: number(p.get("x")).ok_or_else(|| {
                        LayoutValidationFailure::panel("POLYGON", panel_index, "polygon")
                    })?,
                    y: number(p.get("y")).ok_or_else(|| {
                        LayoutValidationFailure::panel("POLYGON", panel_index, "polygon")
                    })?,
                };
                if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
                    return Err(LayoutValidationFailure::panel(
                        "POLYGON",
                        panel_index,
                        "polygon",
                    ));
                };
                Ok(point)
            })
            .collect::<Result<Vec<_>, LayoutValidationFailure>>()?;
        if !strictly_convex(&points) || polygon_area(&points) < min_area {
            return Err(LayoutValidationFailure::panel(
                "POLYGON_TOPOLOGY",
                panel_index,
                "polygon",
            ));
        };
        let minx = points.iter().map(|p| p.x).fold(1.0, f64::min);
        let maxx = points.iter().map(|p| p.x).fold(0.0, f64::max);
        let miny = points.iter().map(|p| p.y).fold(1.0, f64::min);
        let maxy = points.iter().map(|p| p.y).fold(0.0, f64::max);
        if maxx - minx < min_width || maxy - miny < min_height {
            return Err(LayoutValidationFailure::panel(
                "PANEL_SIZE",
                panel_index,
                "polygon",
            ));
        };
        let bounds = raw
            .get("bounds")
            .ok_or_else(|| LayoutValidationFailure::panel("BOUNDS", panel_index, "bounds"))?;
        let derived = json!({"x":minx,"y":miny,"width":maxx-minx,"height":maxy-miny});
        if ["x", "y", "width", "height"].iter().any(|key| {
            match (number(bounds.get(key)), number(derived.get(key))) {
                (Some(actual), Some(expected)) => (actual - expected).abs() > 1e-6,
                _ => true,
            }
        }) {
            return Err(LayoutValidationFailure::panel(
                "BOUNDS",
                panel_index,
                "bounds",
            ));
        };
        let text = raw
            .get("textZone")
            .ok_or_else(|| LayoutValidationFailure::panel("TEXT_ZONE", panel_index, "textZone"))?;
        if number(text.get("width"))
            .filter(|v| *v >= min_text_w)
            .is_none()
            || number(text.get("height"))
                .filter(|v| *v >= min_text_h)
                .is_none()
            || !bounds_contains(safe, text)
        {
            return Err(LayoutValidationFailure::panel(
                "TEXT_ZONE",
                panel_index,
                "textZone",
            ));
        };
        let tx = number(text.get("x"))
            .ok_or_else(|| LayoutValidationFailure::panel("TEXT_ZONE", panel_index, "textZone"))?;
        let ty = number(text.get("y"))
            .ok_or_else(|| LayoutValidationFailure::panel("TEXT_ZONE", panel_index, "textZone"))?;
        let tw = number(text.get("width"))
            .ok_or_else(|| LayoutValidationFailure::panel("TEXT_ZONE", panel_index, "textZone"))?;
        let th = number(text.get("height"))
            .ok_or_else(|| LayoutValidationFailure::panel("TEXT_ZONE", panel_index, "textZone"))?;
        if ![
            Point { x: tx, y: ty },
            Point { x: tx + tw, y: ty },
            Point { x: tx, y: ty + th },
            Point {
                x: tx + tw,
                y: ty + th,
            },
        ]
        .iter()
        .all(|p| point_in_polygon(*p, &points))
        {
            return Err(LayoutValidationFailure::panel(
                "TEXT_ZONE_POLYGON",
                panel_index,
                "textZone",
            ));
        };
        let bleed = raw.get("bleed").and_then(Value::as_object);
        let has_bleed = |edge: &str| {
            bleed
                .and_then(|value| value.get(edge))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let touches_top = miny <= 1e-6;
        let touches_right = 1.0 - maxx <= 1e-6;
        let touches_bottom = 1.0 - maxy <= 1e-6;
        let touches_left = minx <= 1e-6;
        if has_bleed("top") != touches_top
            || has_bleed("right") != touches_right
            || has_bleed("bottom") != touches_bottom
            || has_bleed("left") != touches_left
        {
            return Err(LayoutValidationFailure::panel(
                "BLEED",
                panel_index,
                "bleed",
            ));
        }
        validated.push(GeometryPanel {
            panel_no: no,
            polygon: points,
            parent_panel_no: raw.get("parentPanelNo").and_then(Value::as_i64),
            bleed_top: touches_top,
            bleed_right: touches_right,
            bleed_bottom: touches_bottom,
            bleed_left: touches_left,
        });
    }
    if validated
        .iter()
        .map(|panel| panel.panel_no)
        .collect::<std::collections::BTreeSet<_>>()
        != actual
    {
        return Err(LayoutValidationFailure::layout(
            "PANEL_IDENTITY",
            "geometry.panels",
        ));
    }
    for (panel_index, panel) in validated.iter().enumerate() {
        if let Some(parent_no) = panel.parent_panel_no {
            let parent = validated
                .iter()
                .find(|candidate| candidate.panel_no == parent_no)
                .filter(|parent| parent.panel_no != panel.panel_no)
                .ok_or_else(|| {
                    LayoutValidationFailure::panel("PARENT", panel_index, "parentPanelNo")
                })?;
            if !panel
                .polygon
                .iter()
                .all(|point| point_in_or_on_polygon(*point, &parent.polygon))
            {
                return Err(LayoutValidationFailure::panel(
                    "PARENT",
                    panel_index,
                    "parentPanelNo",
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::from([panel.panel_no]);
        let mut current = panel.parent_panel_no;
        while let Some(parent_no) = current {
            if !seen.insert(parent_no) {
                return Err(LayoutValidationFailure::panel(
                    "PARENT_CYCLE",
                    panel_index,
                    "parentPanelNo",
                ));
            }
            current = validated
                .iter()
                .find(|candidate| candidate.panel_no == parent_no)
                .and_then(|parent| parent.parent_panel_no);
        }
        let _ = (
            panel.bleed_top,
            panel.bleed_right,
            panel.bleed_bottom,
            panel.bleed_left,
        );
    }
    for (index, left) in validated.iter().enumerate() {
        for right in validated.iter().skip(index + 1) {
            let nested = left.parent_panel_no == Some(right.panel_no)
                || right.parent_panel_no == Some(left.panel_no);
            if !nested
                && (polygons_overlap(&left.polygon, &right.polygon)
                    || polygon_distance(&left.polygon, &right.polygon)
                        < number(geometry.get("gutter")).ok_or_else(|| {
                            LayoutValidationFailure::layout("GUTTER", "geometry.gutter")
                        })? - 1e-6)
            {
                return Err(LayoutValidationFailure::panel(
                    "OVERLAP_OR_GUTTER",
                    index,
                    "polygon",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_page_layout(layout: &Value, panels: &[Value]) -> Result<(), String> {
    validate_page_layout_detail(layout, panels).map_err(|_| "LAYOUT_INVALID".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationListInput {
    pub project_id: String,
    pub novel_work_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adaptation {
    pub id: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub title: String,
    pub status: String,
    pub optimistic_version: i64,
    pub current_plan_version_id: Option<String>,
    pub current_continuity_version_id: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationCreateInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub title: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub fn novel_adaptation_list(
    state: tauri::State<'_, DbState>,
    input: AdaptationListInput,
) -> Result<Vec<Adaptation>, String> {
    db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        let mut statement=conn.prepare("SELECT a.id,a.project_id,a.novel_work_id,a.title,a.status,a.optimistic_version,(SELECT id FROM comic_adaptation_plan_heads head WHERE head.comic_adaptation_id=a.id AND head.status='active'),a.current_continuity_version_id FROM comic_adaptations a WHERE a.novel_work_id=? ORDER BY a.created_at,a.id").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map(params![work.id], |r| {
                Ok(Adaptation {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    novel_work_id: r.get(2)?,
                    title: r.get(3)?,
                    status: r.get(4)?,
                    optimistic_version: r.get(5)?,
                    current_plan_version_id: r.get(6)?,
                    current_continuity_version_id: r.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    })
}
#[tauri::command]
pub fn novel_adaptation_create(
    state: tauri::State<'_, DbState>,
    input: AdaptationCreateInput,
) -> Result<Adaptation, String> {
    db::with_connection(&state, |conn| novel_adaptation_create_inner(conn, input))
}

fn novel_adaptation_create_inner(
    conn: &Connection,
    input: AdaptationCreateInput,
) -> Result<Adaptation, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    if input.title.trim().is_empty() {
        return Err("title 不能为空".into());
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let fingerprint = request_hash(&json!({
        "projectId": input.project_id,
        "novelWorkId": input.novel_work_id,
        "title": input.title.trim(),
    }))?;
    let old: Option<(String, String)> = tx
            .query_row(
                "SELECT request_hash,response_json FROM novel_operation_receipts WHERE command_name='novel_adaptation_create' AND idempotency_key=?",
                params![input.idempotency_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
    if let Some((old_hash, raw)) = old {
        if old_hash != fingerprint {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        return serde_json::from_str(&raw).map_err(|e| e.to_string());
    }
    let id = new_id("nadaptation");
    let continuity = new_id("ncontinuity");
    let item = Adaptation {
        id: id.clone(),
        project_id: input.project_id.clone(),
        novel_work_id: work.id.clone(),
        title: input.title.trim().into(),
        status: "active".into(),
        optimistic_version: 0,
        current_plan_version_id: None,
        current_continuity_version_id: Some(continuity.clone()),
    };
    let ts = now();
    tx.execute("INSERT INTO comic_adaptations (id,project_id,novel_work_id,title,status,config_json,current_continuity_version_id,optimistic_version,created_at,updated_at) VALUES (?,?,?,?,'active','{}',?,0,?,?)",params![id,input.project_id,work.id,item.title,continuity,ts,ts]).map_err(|e|e.to_string())?;
    tx.execute("INSERT INTO continuity_state_versions (id,comic_adaptation_id,version,parent_version_id,through_comic_chapter_id,body_json,created_at) VALUES (?,?,0,NULL,NULL,'{}',?)",params![continuity,id,ts]).map_err(|e|e.to_string())?;
    let response = serde_json::to_string(&item).map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?, 'novel_adaptation_create', ?, ?, ?, ?)",params![new_id("nreceipt"),input.idempotency_key,fingerprint,response,ts]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(item)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAcceptPreviewInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub adaptation_proposal_revision_id: String,
    pub comic_chapter_plan_revision_id: String,
    pub scene_plan_revision_id: String,
    pub base_canon_version_id: Option<String>,
    pub base_continuity_version_id: Option<String>,
    pub expected_adaptation_version: Option<i64>,
    pub idempotency_key: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAcceptInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub operation_id: String,
    pub approval_token: String,
    pub idempotency_key: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreview {
    pub operation_id: String,
    pub comic_adaptation_id: String,
    pub approval_token: String,
    pub preview_fingerprint: String,
    pub base_canon_version_id: String,
    pub base_continuity_version_id: String,
    pub expected_adaptation_version: i64,
    pub expires_at: i64,
    pub planning_chapter_keys: Vec<String>,
    pub planning_chapter_summaries: Vec<PlanningChapterSummary>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningSourceRangeDto {
    pub novel_chapter_revision_id: String,
    pub start_utf8_byte: i64,
    pub end_utf8_byte: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningChapterSummary {
    pub planning_chapter_stable_key: String,
    pub source_revision_ids: Vec<String>,
    pub source_ranges: Vec<PlanningSourceRangeDto>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub operation_id: String,
    pub status: String,
    pub receipt_id: Option<String>,
    pub entity_map: Vec<ApplyEntityMap>,
    pub safe_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEntityMap {
    pub entity_kind: String,
    pub stable_key: String,
    pub entity_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneContextResolveInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub scene_plan_revision_id: String,
    pub planning_scene_stable_key: String,
    pub working_context_revision_id: Option<String>,
    pub canon_version_id: String,
    pub novel_state_version_id: String,
    pub continuity_version_id: Option<String>,
    pub adaptation_plan_revision_id: String,
    #[serde(default)]
    pub selected_entity_ids: Vec<String>,
    #[serde(default)]
    pub visual_card_revision_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneContextApproveInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub scene_context_snapshot_id: String,
    pub context_fingerprint: String,
    pub idempotency_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneContextSnapshot {
    pub snapshot_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub scene_plan_revision_id: String,
    pub planning_scene_stable_key: String,
    pub working_context_revision_id: Option<String>,
    pub canon_version_id: String,
    pub novel_state_version_id: String,
    pub continuity_version_id: String,
    pub adaptation_plan_revision_id: String,
    pub context_fingerprint: String,
    pub status: String,
    pub resolved_context: Value,
    pub selected_entity_ids: Vec<String>,
    pub visual_card_revision_ids: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanSelection {
    pub planning_scene_stable_key: String,
    pub scene_context_snapshot_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanApplyPreviewInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub accepted_plan_version_id: String,
    pub page_panel_plan_revision_id: String,
    pub scene_context_selections: Vec<ComicPlanSelection>,
    pub base_canon_version_id: String,
    pub base_continuity_version_id: String,
    pub expected_adaptation_version: i64,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanApplyInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub operation_id: String,
    pub approval_token: String,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicApplyOperationInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub operation_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanCounts {
    pub chapters: usize,
    pub scenes: usize,
    pub pages: usize,
    pub panels: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanPreview {
    pub operation_id: String,
    pub comic_adaptation_id: String,
    pub approval_token: String,
    pub preview_fingerprint: String,
    pub expected_adaptation_version: i64,
    pub tree: Value,
    pub layout: Value,
    pub counts: ComicPlanCounts,
    pub scene_context_selections: Vec<ComicPlanSelection>,
    pub expires_at: i64,
}

fn adopted(
    conn: &Connection,
    adaptation: &str,
    revision: &str,
    kind: &str,
) -> Result<Value, String> {
    conn.query_row("SELECT revision.body_json FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE revision.id=? AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.comic_adaptation_id=? AND artifact.artifact_type=?",params![revision,adaptation,kind],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?.map(|raw|serde_json::from_str(&raw).map_err(|_|String::from("SCHEMA_INVALID"))).transpose()?.ok_or(String::from("REVISION_CONFLICT"))
}
fn chapter_keys_and_ranges(body: &Value) -> Result<Vec<PlanningChapterRanges>, String> {
    let chapters = body
        .get("chapters")
        .and_then(Value::as_array)
        .ok_or("SCHEMA_INVALID")?;
    chapters
        .iter()
        .map(|chapter| {
            let key = chapter
                .get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or("SCHEMA_INVALID")?
                .to_owned();
            let ranges = chapter
                .get("sourceSelections")
                .or_else(|| chapter.get("sourceRanges"))
                .and_then(Value::as_array)
                .ok_or("EVIDENCE_RANGE_INVALID")?
                .iter()
                .map(|range| {
                    Ok((
                        range
                            .get("novelChapterRevisionId")
                            .or_else(|| range.get("revisionId"))
                            .and_then(Value::as_str)
                            .ok_or("EVIDENCE_RANGE_INVALID")?
                            .to_owned(),
                        range
                            .get("start")
                            .or_else(|| range.get("startUtf8Byte"))
                            .or_else(|| range.get("sourceStart"))
                            .and_then(Value::as_i64)
                            .ok_or("EVIDENCE_RANGE_INVALID")?,
                        range
                            .get("end")
                            .or_else(|| range.get("endUtf8Byte"))
                            .or_else(|| range.get("sourceEnd"))
                            .and_then(Value::as_i64)
                            .ok_or("EVIDENCE_RANGE_INVALID")?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if ranges.is_empty() {
                return Err("EVIDENCE_RANGE_INVALID".into());
            }
            Ok((key, ranges))
        })
        .collect()
}

/// Source selections are persisted as UTF-8 byte offsets. Validate them against
/// the immutable revision before a completed analysis can become reviewable or
/// an accepted plan can write planning sources.
pub(crate) fn validate_comic_chapter_plan_evidence_ranges(
    conn: &Connection,
    novel_work_id: &str,
    chapter_plan: &Value,
) -> Result<(), String> {
    for (_, ranges) in chapter_keys_and_ranges(chapter_plan)? {
        for (revision_id, start, end) in ranges {
            let content = conn
                .query_row(
                    "SELECT revision.content
                     FROM novel_chapter_revisions revision
                     JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
                     WHERE revision.id=? AND chapter.novel_work_id=?",
                    params![revision_id, novel_work_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|_| "EVIDENCE_SOURCE_READ_FAILED")?
                .ok_or("EVIDENCE_SOURCE_SCOPE_INVALID")?;
            let start = usize::try_from(start).map_err(|_| "EVIDENCE_RANGE_INVALID")?;
            let end = usize::try_from(end).map_err(|_| "EVIDENCE_RANGE_INVALID")?;
            if start >= end
                || end > content.len()
                || !content.is_char_boundary(start)
                || !content.is_char_boundary(end)
            {
                return Err("EVIDENCE_RANGE_INVALID".into());
            }
        }
    }
    Ok(())
}

fn validate_v4_comic_chapter_plan_source_options(
    conn: &Connection,
    run: &AdaptationAnalysisRun,
    chapter_plan: &Value,
) -> Result<(), String> {
    let prompt_version: String = conn
        .query_row(
            "SELECT prompt_version FROM adaptation_analysis_runs WHERE id=?",
            params![run.id],
            |row| row.get(0),
        )
        .map_err(|_| "FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?;
    if !adaptation_prompt_uses_source_range_options(&prompt_version) {
        return Ok(());
    }
    let prompt = adaptation_analysis_prompt(conn, &run.id)?;
    let options = prompt
        .source_range_options
        .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?;
    let allowed = options
        .iter()
        .map(|option| {
            Ok((
                option
                    .get("novelChapterRevisionId")
                    .and_then(Value::as_str)
                    .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?
                    .to_owned(),
                option
                    .get("startUtf8Byte")
                    .and_then(Value::as_i64)
                    .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?,
                option
                    .get("endUtf8Byte")
                    .and_then(Value::as_i64)
                    .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?,
            ))
        })
        .collect::<Result<std::collections::BTreeSet<_>, String>>()?;
    for (_, ranges) in chapter_keys_and_ranges(chapter_plan)? {
        if ranges.iter().any(|range| !allowed.contains(range)) {
            return Err("EVIDENCE_RANGE_OPTION_MISMATCH".into());
        }
    }
    Ok(())
}

fn adaptation_belongs_to_work(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    adaptation_id: &str,
) -> Result<(), String> {
    let owned: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM comic_adaptations WHERE id=? AND project_id=? AND novel_work_id=? AND status='active')",
            params![adaptation_id, project_id, work_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if owned {
        Ok(())
    } else {
        Err("OWNER_MISMATCH".into())
    }
}

fn ensure_continuity_baseline(tx: &Transaction<'_>, adaptation_id: &str) -> Result<String, String> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT current_continuity_version_id FROM comic_adaptations WHERE id=?",
            params![adaptation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if let Some(id) = existing {
        let valid: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM continuity_state_versions WHERE id=? AND comic_adaptation_id=? AND version>=0)",
                params![id, adaptation_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        return valid
            .then_some(id)
            .ok_or_else(|| "CONTINUITY_BASELINE_INVALID".into());
    }
    let has_versions: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM continuity_state_versions WHERE comic_adaptation_id=?)",
            params![adaptation_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if has_versions {
        return Err("CONTINUITY_BASELINE_REQUIRED".into());
    }
    let id = new_id("continuity");
    let ts = now();
    tx.execute(
        "INSERT INTO continuity_state_versions (id,comic_adaptation_id,version,parent_version_id,through_comic_chapter_id,body_json,created_at) VALUES (?,?,0,NULL,NULL,'{}',?)",
        params![id, adaptation_id, ts],
    )
    .map_err(|e| e.to_string())?;
    let changed = tx
        .execute(
            "UPDATE comic_adaptations SET current_continuity_version_id=?, optimistic_version=optimistic_version+1, updated_at=? WHERE id=? AND current_continuity_version_id IS NULL",
            params![id, ts, adaptation_id],
        )
        .map_err(|e| e.to_string())?;
    if changed != 1 {
        return Err("CONTINUITY_BASELINE_CONFLICT".into());
    }
    Ok(id)
}

fn json_id_list(value: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(value).map_err(|_| "SCHEMA_INVALID".into())
}

fn snapshot_value(tx: &Transaction<'_>, id: &str) -> Result<SceneContextSnapshot, String> {
    let row: SnapshotRow = tx
        .query_row(
            "SELECT novel_work_id,comic_adaptation_id,scene_plan_revision_id,planning_scene_stable_key,working_context_revision_id,novel_canon_version_id,novel_state_version_id,continuity_state_version_id,adaptation_plan_revision_id,context_fingerprint,status FROM comic_scene_context_snapshots WHERE id=?",
            params![id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "SNAPSHOT_UNKNOWN".to_string())?;
    let resolved_raw: String = tx
        .query_row(
            "SELECT resolved_context_json FROM comic_scene_context_snapshots WHERE id=?",
            params![id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let selected_raw: String = tx.query_row("SELECT COALESCE(json_group_array(novel_entity_id),'[]') FROM comic_scene_context_entities WHERE scene_context_snapshot_id=? ORDER BY source_order,novel_entity_id",params![id],|r|r.get(0)).map_err(|e|e.to_string())?;
    let cards_raw: String = tx.query_row("SELECT COALESCE(json_group_array(comic_card_revision_id),'[]') FROM comic_scene_context_visual_cards WHERE scene_context_snapshot_id=? ORDER BY source_order,comic_card_revision_id",params![id],|r|r.get(0)).map_err(|e|e.to_string())?;
    Ok(SceneContextSnapshot {
        snapshot_id: id.into(),
        novel_work_id: row.0,
        comic_adaptation_id: row.1,
        scene_plan_revision_id: row.2,
        planning_scene_stable_key: row.3,
        working_context_revision_id: row.4,
        canon_version_id: row.5,
        novel_state_version_id: row.6,
        continuity_version_id: row.7,
        adaptation_plan_revision_id: row.8,
        context_fingerprint: row.9,
        status: row.10,
        resolved_context: serde_json::from_str(&resolved_raw)
            .map_err(|_| "SCHEMA_INVALID".to_string())?,
        selected_entity_ids: json_id_list(&selected_raw)?,
        visual_card_revision_ids: json_id_list(&cards_raw)?,
    })
}

fn receipt_entity_map(
    tx: &Transaction<'_>,
    operation_id: &str,
) -> Result<Vec<ApplyEntityMap>, String> {
    let mut statement = tx
        .prepare("SELECT entity_kind,stable_key,entity_id FROM analysis_apply_receipt_entity_maps WHERE analysis_apply_operation_id=? ORDER BY entity_kind,stable_key")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(params![operation_id], |r| {
            Ok(ApplyEntityMap {
                entity_kind: r.get(0)?,
                stable_key: r.get(1)?,
                entity_id: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn page_plan_shape(body: &Value) -> Result<(String, Vec<Value>, ComicPlanCounts), String> {
    page_plan_shape_with_layout_validation(body, |layout, panels, _| {
        validate_page_layout(layout, panels)
    })
}

fn page_plan_shape_for_adaptation_analysis(
    body: &Value,
) -> Result<(String, Vec<Value>, ComicPlanCounts), String> {
    page_plan_shape_with_layout_validation(body, |layout, panels, page_index| {
        validate_page_layout_detail(layout, panels)
            .map_err(|failure| failure.adaptation_diagnostic(page_index))
    })
}

fn page_plan_shape_with_layout_validation(
    body: &Value,
    mut validate_layout: impl FnMut(&Value, &[Value], usize) -> Result<(), String>,
) -> Result<(String, Vec<Value>, ComicPlanCounts), String> {
    let chapter_key = body
        .get("comicChapterDraftId")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or("SCHEMA_INVALID")?
        .to_owned();
    let pages = body
        .get("pages")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or("SCHEMA_INVALID")?
        .clone();
    let mut panels = 0;
    let mut seen_pages = std::collections::BTreeSet::new();
    let mut seen_panel_keys = std::collections::BTreeSet::new();
    for (page_index, page) in pages.iter().enumerate() {
        let key = page
            .get("stableKey")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or("SCHEMA_INVALID")?;
        let page_no = page
            .get("pageNo")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .ok_or("SCHEMA_INVALID")?;
        if !seen_pages.insert((key.to_owned(), page_no))
            || !page.get("layout").is_some_and(Value::is_object)
        {
            return Err("SCHEMA_INVALID".into());
        }
        let page_panels = page
            .get("panels")
            .and_then(Value::as_array)
            .ok_or("SCHEMA_INVALID")?;
        validate_layout(
            page.get("layout").ok_or("LAYOUT_INVALID")?,
            page_panels,
            page_index,
        )?;
        for panel in page_panels {
            let key = panel
                .get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or("SCHEMA_INVALID")?;
            if panel
                .get("panelNo")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
                .is_none()
                || !seen_panel_keys.insert(key.to_owned())
            {
                return Err("SCHEMA_INVALID".into());
            }
            panels += 1;
        }
    }
    Ok((
        chapter_key,
        pages.clone(),
        ComicPlanCounts {
            chapters: 1,
            scenes: 0,
            pages: pages.len(),
            panels,
        },
    ))
}

// Adaptation analysis deliberately has a separate run model from the legacy
// fourteen-output source analysis.  A run is a frozen transformation from ten
// already-versioned source artifacts into exactly four adaptation artifacts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisRun {
    pub id: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub comic_adaptation_chapter_id: String,
    pub source_analysis_run_id: Option<String>,
    pub base_canon_version_id: String,
    pub base_novel_state_version_id: Option<String>,
    pub base_continuity_version_id: String,
    pub status: String,
    pub attempt_no: i64,
    pub safe_error_code: Option<String>,
    pub safe_user_message: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisStartInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    /// Existing explicit mapping.  When omitted the caller must provide
    /// `novelChapterRevisionId`; the transaction creates/selects that exact
    /// adaptation chapter mapping, never a "latest" chapter.
    pub comic_adaptation_chapter_id: Option<String>,
    pub source_analysis_run_id: Option<String>,
    /// Required when `sourceAnalysisRunId` is absent.  The ten IDs are frozen
    /// as ordered input rows, never resolved again from a mutable head.
    #[serde(default)]
    pub source_artifact_revision_ids: Vec<String>,
    pub novel_chapter_revision_id: Option<String>,
    pub base_canon_version_id: String,
    pub base_novel_state_version_id: Option<String>,
    pub base_continuity_version_id: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// Optional immutable production-level page contract. Individual page
    /// dialogue fields retain the three states: omitted, explicitly empty, or
    /// exact ordered dialogue.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub comic_plan_intent: Option<ComicPlanIntent>,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisStatusInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub adaptation_analysis_run_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisListInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub comic_adaptation_chapter_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisRetryInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub adaptation_analysis_run_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationAnalysisRecoverInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AdaptationAnalysisStartRecord {
    pub(crate) run: AdaptationAnalysisRun,
    // Stored receipts intentionally omit this process-local side-effect flag.
    #[serde(skip, default)]
    pub(crate) dispatch: bool,
}

fn adaptation_analysis_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AdaptationAnalysisRun> {
    Ok(AdaptationAnalysisRun {
        id: row.get(0)?,
        project_id: row.get(1)?,
        novel_work_id: row.get(2)?,
        comic_adaptation_id: row.get(3)?,
        comic_adaptation_chapter_id: row.get(4)?,
        source_analysis_run_id: row.get(5)?,
        base_canon_version_id: row.get(6)?,
        base_novel_state_version_id: row.get(7)?,
        base_continuity_version_id: row.get(8)?,
        status: row.get(9)?,
        attempt_no: row.get(10)?,
        safe_error_code: row.get(11)?,
        safe_user_message: row.get(12)?,
    })
}

const ADAPTATION_RUN_SELECT: &str = "SELECT id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,status,attempt_no,safe_error_code,safe_user_message FROM adaptation_analysis_runs";

fn active_work_in_tx(tx: &Transaction<'_>, project_id: &str, work_id: &str) -> Result<(), String> {
    let active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM novel_works WHERE id=? AND project_id=? AND status='active')",
        params![work_id, project_id], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    active
        .then_some(())
        .ok_or_else(|| "NOVEL_WORK_ARCHIVED".into())
}

fn adaptation_chapter_revision(
    tx: &Transaction<'_>,
    project_id: &str,
    work_id: &str,
    adaptation_id: &str,
    chapter_id: &str,
) -> Result<String, String> {
    tx.query_row(
        "SELECT chapter.novel_chapter_revision_id FROM comic_adaptation_chapters chapter JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id WHERE chapter.id=? AND chapter.comic_adaptation_id=? AND adaptation.project_id=? AND adaptation.novel_work_id=? AND adaptation.status='active'",
        params![chapter_id, adaptation_id, project_id, work_id], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())?.ok_or_else(|| "OWNER_MISMATCH".into())
}

fn resolve_adaptation_chapter(
    tx: &Transaction<'_>,
    project_id: &str,
    work_id: &str,
    adaptation_id: &str,
    supplied_chapter_id: Option<&str>,
    supplied_revision_id: Option<&str>,
) -> Result<(String, String), String> {
    if let Some(chapter_id) = supplied_chapter_id {
        let revision =
            adaptation_chapter_revision(tx, project_id, work_id, adaptation_id, chapter_id)?;
        if supplied_revision_id.is_some_and(|value| value != revision) {
            return Err("novelChapterRevisionId 与 comicAdaptationChapterId 不匹配".into());
        }
        return Ok((chapter_id.to_owned(), revision));
    }
    let revision_id = supplied_revision_id
        .ok_or("comicAdaptationChapterId 缺失时 novelChapterRevisionId 必填")?;
    let sequence: i64 = tx.query_row(
        "SELECT chapter.sequence_no FROM novel_chapter_revisions revision JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id WHERE revision.id=? AND chapter.novel_work_id=?",
        params![revision_id, work_id], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())?.ok_or_else(|| "novelChapterRevisionId 不属于当前小说".to_string())?;
    let id = new_id("adaptchapter");
    tx.execute(
        "INSERT OR IGNORE INTO comic_adaptation_chapters (id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES (?,?,?,?,?)",
        params![id, adaptation_id, revision_id, sequence, now()],
    ).map_err(|e| e.to_string())?;
    let resolved: Option<String> = tx.query_row(
        "SELECT id FROM comic_adaptation_chapters WHERE comic_adaptation_id=? AND novel_chapter_revision_id=?",
        params![adaptation_id, revision_id], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())?;
    resolved
        .map(|chapter| (chapter, revision_id.to_owned()))
        .ok_or_else(|| "ADAPTATION_CHAPTER_SEQUENCE_CONFLICT".into())
}

fn frozen_source_inputs(
    tx: &Transaction<'_>,
    input: &AdaptationAnalysisStartInput,
) -> Result<FrozenSourceInputs, String> {
    if let Some(source_run) = input.source_analysis_run_id.as_deref() {
        if !input.source_artifact_revision_ids.is_empty() {
            return Err("sourceAnalysisRunId 与 sourceArtifactRevisionIds 只能二选一".into());
        }
        let mut values = Vec::with_capacity(ORIGINAL_ARTIFACT_TYPES.len());
        for (order, kind) in ORIGINAL_ARTIFACT_TYPES.iter().enumerate() {
            let row: Option<(String, String)> = tx.query_row(
                "SELECT revision.id,revision.body_json FROM analysis_artifacts artifact JOIN analysis_artifact_revisions revision ON revision.id=artifact.candidate_head_revision_id WHERE artifact.source_analysis_run_id=? AND artifact.novel_work_id=? AND artifact.artifact_type=? AND revision.status IN ('candidate','adopted')",
                params![source_run, input.novel_work_id, kind], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().map_err(|e| e.to_string())?;
            let (revision, body) = row.ok_or_else(|| format!("SOURCE_ARTIFACT_MISSING:{kind}"))?;
            values.push((
                kind.to_string(),
                revision,
                format!("sha256:{:x}", Sha256::digest(body.as_bytes())),
            ));
            debug_assert_eq!(order, values.len() - 1);
        }
        Ok(("source_run".into(), Some(source_run.to_string()), values))
    } else {
        if input.source_artifact_revision_ids.len() != ORIGINAL_ARTIFACT_TYPES.len() {
            return Err("artifact_revisions 模式必须显式提供十个原著产物 revision".into());
        }
        let mut values = Vec::with_capacity(ORIGINAL_ARTIFACT_TYPES.len());
        let mut seen = std::collections::BTreeSet::new();
        for (order, revision) in input.source_artifact_revision_ids.iter().enumerate() {
            let row: Option<(String, String)> = tx.query_row(
                "SELECT artifact.artifact_type,revision.body_json FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE revision.id=? AND artifact.novel_work_id=? AND artifact.adopted_head_revision_id=revision.id AND revision.status='adopted'",
                params![revision, input.novel_work_id], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().map_err(|e| e.to_string())?;
            let (kind, body) = row.ok_or_else(|| "SOURCE_ARTIFACT_REVISION_INVALID".to_string())?;
            if !ORIGINAL_ARTIFACT_TYPES.contains(&kind.as_str()) || !seen.insert(kind.clone()) {
                return Err("sourceArtifactRevisionIds 必须恰好覆盖十个不同原著类型".into());
            }
            values.push((
                kind,
                revision.clone(),
                format!("sha256:{:x}", Sha256::digest(body.as_bytes())),
            ));
            debug_assert!(order < ORIGINAL_ARTIFACT_TYPES.len());
        }
        values.sort_by_key(|(kind, _, _)| {
            ORIGINAL_ARTIFACT_TYPES
                .iter()
                .position(|value| *value == kind)
                .unwrap_or(usize::MAX)
        });
        if values
            .iter()
            .map(|(kind, _, _)| kind.as_str())
            .collect::<Vec<_>>()
            != ORIGINAL_ARTIFACT_TYPES
        {
            return Err("sourceArtifactRevisionIds 缺少原著类型".into());
        }
        Ok(("artifact_revisions".into(), None, values))
    }
}

fn frozen_source_range_options(
    conn: &Connection,
    novel_work_id: &str,
    chapter_revision_id: &str,
    frozen_input_revision_ids: &[String],
) -> Result<Vec<Value>, String> {
    let mut revision_ids = std::collections::BTreeSet::new();
    revision_ids.insert(chapter_revision_id.to_owned());
    for input_revision_id in frozen_input_revision_ids {
        let source_revision_id: Option<String> = conn
            .query_row(
                "SELECT artifact.novel_chapter_revision_id
                 FROM analysis_artifact_revisions revision
                 JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
                 WHERE revision.id=? AND artifact.novel_work_id=?",
                params![input_revision_id, novel_work_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?
            .flatten();
        if let Some(source_revision_id) = source_revision_id {
            revision_ids.insert(source_revision_id);
        }
    }
    revision_ids
        .into_iter()
        .map(|revision_id| {
            let content: String = conn
                .query_row(
                    "SELECT revision.content
                     FROM novel_chapter_revisions revision
                     JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
                     WHERE revision.id=? AND chapter.novel_work_id=?",
                    params![revision_id, novel_work_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| "FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?
                .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?;
            if content.is_empty() {
                return Err("FROZEN_SOURCE_RANGE_OPTIONS_INVALID".into());
            }
            Ok(json!({
                "novelChapterRevisionId": revision_id,
                "startUtf8Byte": 0,
                "endUtf8Byte": content.len() as i64,
                "verifiedExcerpt": content.chars().take(240).collect::<String>(),
            }))
        })
        .collect()
}

pub(crate) fn start_adaptation_analysis_inner(
    conn: &Connection,
    mut input: AdaptationAnalysisStartInput,
    configured: bool,
    provider_id: String,
    model_id: String,
    owner: &str,
) -> Result<AdaptationAnalysisStartRecord, String> {
    input.comic_plan_intent = canonicalize_comic_plan_intent(input.comic_plan_intent.take())?;
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let frozen_comic_plan_intent = input.comic_plan_intent.clone();
    let frozen_comic_plan_constraints = frozen_comic_plan_intent
        .as_ref()
        .map(comic_plan_intent_constraints);
    let system_prompt_hash = adaptation_analysis_system_prompt_hash_for_frozen_constraints(
        ADAPTATION_ANALYSIS_PROMPT_VERSION,
        frozen_comic_plan_constraints.as_ref(),
    )?;
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    let key = input.idempotency_key.clone();
    with_receipt(
        conn,
        "novel_adaptation_analysis_start",
        &key,
        &request,
        move |tx| {
            active_work_in_tx(tx, &input.project_id, &work.id)?;
            let (adaptation_chapter_id, chapter_revision) = resolve_adaptation_chapter(
                tx,
                &input.project_id,
                &work.id,
                &input.comic_adaptation_id,
                input.comic_adaptation_chapter_id.as_deref(),
                input.novel_chapter_revision_id.as_deref(),
            )?;
            let (mode, source_run, frozen_inputs) = frozen_source_inputs(tx, &input)?;
            let frozen_input_revision_ids = frozen_inputs
                .iter()
                .map(|(_, revision_id, _)| revision_id.clone())
                .collect::<Vec<_>>();
            let source_range_options = frozen_source_range_options(
                tx,
                &work.id,
                &chapter_revision,
                &frozen_input_revision_ids,
            )?;
            if source_run.is_some() {
                let source_chapter: String = tx
                    .query_row(
                        "SELECT novel_chapter_revision_id FROM source_analysis_runs WHERE id=?",
                        params![source_run],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                if source_chapter != chapter_revision {
                    return Err("SOURCE_ANALYSIS_CHAPTER_MISMATCH".into());
                }
            }
            let baselines_valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM novel_canon_versions WHERE id=? AND novel_work_id=?) AND (? IS NULL OR EXISTS(SELECT 1 FROM novel_state_versions WHERE id=? AND novel_work_id=?)) AND EXISTS(SELECT 1 FROM continuity_state_versions WHERE id=? AND comic_adaptation_id=?)",
            params![input.base_canon_version_id,work.id,input.base_novel_state_version_id,input.base_novel_state_version_id,work.id,input.base_continuity_version_id,input.comic_adaptation_id], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
            if !baselines_valid {
                return Err("BASELINE_SCOPE_INVALID".into());
            }
            let frozen_branch_config: Value = tx
                .query_row(
                    "SELECT config_json FROM comic_adaptations WHERE id=? AND novel_work_id=?",
                    params![input.comic_adaptation_id, work.id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .map(json_value)
                .ok_or_else(|| "OWNER_MISMATCH".to_string())?;
            if !frozen_branch_config.is_object() {
                return Err("ADAPTATION_CONFIG_INVALID".into());
            }
            let timestamp = now();
            let run = AdaptationAnalysisRun {
                id: new_id("adaptrun"),
                project_id: input.project_id.clone(),
                novel_work_id: work.id.clone(),
                comic_adaptation_id: input.comic_adaptation_id.clone(),
                comic_adaptation_chapter_id: adaptation_chapter_id,
                source_analysis_run_id: source_run.clone(),
                base_canon_version_id: input.base_canon_version_id.clone(),
                base_novel_state_version_id: input.base_novel_state_version_id.clone(),
                base_continuity_version_id: input.base_continuity_version_id.clone(),
                status: if configured {
                    "running".into()
                } else {
                    "error".into()
                },
                attempt_no: 1,
                safe_error_code: (!configured).then(|| "NOT_CONFIGURED".into()),
                safe_user_message: (!configured).then(|| "未配置 LLM，可配置后重试".into()),
            };
            let output_identity_bindings = adaptation_output_identity_bindings(
                &run.comic_adaptation_id,
                &run.comic_adaptation_chapter_id,
            );
            let fingerprint = request_hash(&json!({
                "mode":mode,"sourceRun":source_run,"inputs":frozen_inputs,"canon":run.base_canon_version_id,
                "state":run.base_novel_state_version_id,"continuity":run.base_continuity_version_id,
                "adaptation":run.comic_adaptation_id,"chapter":run.comic_adaptation_chapter_id,
                "provider":provider_id,"model":model_id,"branchConfig":frozen_branch_config,
                "comicPlanIntent":&frozen_comic_plan_intent,
                "comicPlanIntentConstraints":&frozen_comic_plan_constraints,
                "sourceRangeOptions":&source_range_options,
                "outputIdentityBindings":&output_identity_bindings,
                "promptVersion":ADAPTATION_ANALYSIS_PROMPT_VERSION,
                "systemPromptHash":system_prompt_hash,
                "schemaHash":format!("sha256:{:x}",Sha256::digest(ADAPTATION_ANALYSIS_SCHEMA.as_bytes()))
            }))?;
            tx.execute(
            "INSERT INTO adaptation_analysis_runs (id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,'novel-analysis.v1',?,?, 'draft',1,NULL,NULL,?,?,NULL)",
            params![run.id,run.project_id,run.novel_work_id,run.comic_adaptation_id,run.comic_adaptation_chapter_id,mode,source_run,run.base_canon_version_id,run.base_novel_state_version_id,run.base_continuity_version_id,provider_id,model_id,ADAPTATION_ANALYSIS_PROMPT_VERSION,fingerprint,input.idempotency_key,timestamp,timestamp],
        ).map_err(|e| format!("创建改编分析 run 失败: {e}"))?;
            for (order, (kind, revision, _)) in frozen_inputs.iter().enumerate() {
                tx.execute("INSERT INTO adaptation_analysis_run_inputs (adaptation_analysis_run_id,artifact_type,analysis_artifact_revision_id,source_order) VALUES (?,?,?,?)",params![run.id,kind,revision,order as i64]).map_err(|e|format!("冻结改编分析输入失败: {e}"))?;
            }
            let attempt_id = new_id("adaptattempt");
            if configured {
                let expiry = timestamp + ADAPTATION_ANALYSIS_LEASE_MS;
                tx.execute("INSERT INTO adaptation_analysis_run_attempts (id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,created_at) VALUES (?,?,1,NULL,'queued',?,?,?,?)",params![attempt_id,run.id,owner,expiry,timestamp,timestamp]).map_err(|e|e.to_string())?;
                tx.execute("UPDATE adaptation_analysis_runs SET status='queued',lease_owner=?,lease_expires_at=?,heartbeat_at=?,updated_at=? WHERE id=? AND status='draft'",params![owner,expiry,timestamp,timestamp,run.id]).map_err(|e|e.to_string())?;
                tx.execute("UPDATE adaptation_analysis_run_attempts SET status='running' WHERE id=? AND status='queued'",params![attempt_id]).map_err(|e|e.to_string())?;
                let changed=tx.execute("UPDATE adaptation_analysis_runs SET status='running',updated_at=? WHERE id=? AND status='queued'",params![timestamp,run.id]).map_err(|e|e.to_string())?;
                if changed != 1 {
                    return Err("ADAPTATION_ANALYSIS_STATE_CONFLICT".into());
                }
            } else {
                tx.execute("INSERT INTO adaptation_analysis_run_attempts (id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,safe_error_code,safe_user_message,created_at,finished_at) VALUES (?,?,1,NULL,'error','NOT_CONFIGURED','未配置 LLM',?,?)",params![attempt_id,run.id,timestamp,timestamp]).map_err(|e|e.to_string())?;
                tx.execute("UPDATE adaptation_analysis_runs SET status='error',safe_error_code='NOT_CONFIGURED',safe_user_message='未配置 LLM，可配置后重试',updated_at=?,finished_at=? WHERE id=? AND status='draft'",params![timestamp,timestamp,run.id]).map_err(|e|e.to_string())?;
            }
            tx.execute("INSERT INTO adaptation_analysis_run_events (id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at) VALUES (?,?,1,?, ?,?)",params![new_id("adaptevent"),run.id,if configured {"started"} else {"not_configured"},json!({"status":run.status,"inputMode":mode,"promptVersion":ADAPTATION_ANALYSIS_PROMPT_VERSION,"systemPromptHash":system_prompt_hash,"branchConfig":frozen_branch_config,"comicPlanIntent":&frozen_comic_plan_intent,"comicPlanIntentConstraints":&frozen_comic_plan_constraints,"sourceRangeOptions":&source_range_options,"outputIdentityBindings":&output_identity_bindings}).to_string(),timestamp]).map_err(|e|e.to_string())?;
            Ok(AdaptationAnalysisStartRecord {
                run,
                dispatch: configured,
            })
        },
    )
}

fn adaptation_safe_error(value: &str) -> String {
    crate::logging::error_text(value)
        .chars()
        .take(800)
        .collect()
}

fn adaptation_completion_endpoint(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.into()
    } else {
        format!("{base}/chat/completions")
    }
}

fn parse_adaptation_model_json(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```JSON"))
        .unwrap_or(raw);
    let raw = raw.strip_suffix("```").unwrap_or(raw).trim();
    let raw = raw
        .rfind("</think>")
        .map(|offset| &raw[offset + 8..])
        .unwrap_or(raw);
    serde_json::from_str(raw.trim()).map_err(|_| "模型未返回合法 JSON".into())
}

fn adaptation_owner_type(kind: &str) -> &'static str {
    if kind == "adaptation_proposal" {
        "comic_adaptation"
    } else {
        "comic_chapter"
    }
}

fn nonempty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn evidence_refs_are_valid(value: Option<&Value>) -> bool {
    value.and_then(Value::as_array).is_some_and(|refs| {
        !refs.is_empty()
            && refs.iter().all(|reference| {
                let start = reference.get("startUtf8Byte").and_then(Value::as_i64);
                let end = reference.get("endUtf8Byte").and_then(Value::as_i64);
                nonempty_string(reference.get("novelChapterRevisionId"))
                    && start
                        .zip(end)
                        .is_some_and(|(start, end)| start >= 0 && end > start)
                    && reference
                        .get("confidence")
                        .and_then(Value::as_f64)
                        .is_some_and(|value| (0.0..=1.0).contains(&value))
            })
    })
}

fn collection_has_empty_reason(content: &Value, field: &str) -> bool {
    content
        .get(field)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty() || nonempty_string(content.get("emptyReason")))
}

fn validate_adaptation_content(
    kind: &str,
    content: &Value,
    chapter_id: &str,
) -> Result<(), String> {
    match kind {
        "adaptation_proposal" => {
            let decisions = content
                .get("decisions")
                .and_then(Value::as_array)
                .ok_or("adaptation_proposal 缺少 decisions")?;
            if !collection_has_empty_reason(content, "decisions")
                || !decisions.iter().all(|decision| {
                    nonempty_string(decision.get("stableKey"))
                        && matches!(
                            decision.get("action").and_then(Value::as_str),
                            Some("keep" | "compress" | "merge" | "delay" | "omit")
                        )
                        && nonempty_string(decision.get("rationale"))
                        && evidence_refs_are_valid(decision.get("sourceRanges"))
                        && nonempty_string(decision.get("targetHint"))
                })
            {
                return Err("ADAPTATION_PROPOSAL_INVALID".into());
            }
        }
        "comic_chapter_plan" => {
            let chapters = content
                .get("chapters")
                .and_then(Value::as_array)
                .ok_or("comic_chapter_plan 缺少 chapters")?;
            if !collection_has_empty_reason(content, "chapters")
                || !chapters.iter().all(|chapter| {
                    nonempty_string(chapter.get("stableKey"))
                        && evidence_refs_are_valid(chapter.get("sourceSelections"))
                        && nonempty_string(chapter.get("goal"))
                        && nonempty_string(chapter.get("turn"))
                        && nonempty_string(chapter.get("hook"))
                        && chapter
                            .get("pageBudget")
                            .and_then(Value::as_i64)
                            .is_some_and(|value| value > 0)
                })
            {
                return Err("COMIC_CHAPTER_PLAN_INVALID".into());
            }
            chapter_keys_and_ranges(content)?;
        }
        "scene_plan" => {
            if content.get("comicChapterDraftId").and_then(Value::as_str) != Some(chapter_id) {
                return Err("SCENE_PLAN_CHAPTER_MISMATCH".into());
            }
            let scenes = content
                .get("scenes")
                .and_then(Value::as_array)
                .ok_or("scene_plan 缺少 scenes")?;
            let mut orders = std::collections::BTreeSet::new();
            let mut keys = std::collections::BTreeSet::new();
            if !collection_has_empty_reason(content, "scenes")
                || !scenes.iter().all(|scene| {
                    let order = scene
                        .get("order")
                        .and_then(Value::as_i64)
                        .filter(|value| *value > 0);
                    order.is_some_and(|value| orders.insert(value))
                        && scene
                            .get("stableKey")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .is_some_and(|value| keys.insert(value.to_owned()))
                        && nonempty_string(scene.get("goal"))
                        && scene.get("beats").is_some_and(Value::is_array)
                        && scene.get("locationKeys").is_some_and(Value::is_array)
                        && scene.get("characterKeys").is_some_and(Value::is_array)
                        && scene.get("stateDelta").is_some_and(Value::is_array)
                })
            {
                return Err("SCENE_PLAN_INVALID".into());
            }
        }
        "page_panel_plan" => {
            if content.get("comicChapterDraftId").and_then(Value::as_str) != Some(chapter_id) {
                return Err("PAGE_PLAN_CHAPTER_MISMATCH".into());
            }
            page_plan_shape(content)?;
        }
        _ => return Err("ADAPTATION_ARTIFACT_TYPE_INVALID".into()),
    }
    Ok(())
}

fn validate_adaptation_analysis_content(
    kind: &str,
    content: &Value,
    chapter_id: &str,
) -> Result<(), String> {
    if kind != "page_panel_plan" {
        return validate_adaptation_content(kind, content, chapter_id);
    }
    if content.get("comicChapterDraftId").and_then(Value::as_str) != Some(chapter_id) {
        return Err("PAGE_PLAN_CHAPTER_MISMATCH".into());
    }
    page_plan_shape_for_adaptation_analysis(content)?;
    Ok(())
}

fn page_panel_dialogues(page: &Value) -> Result<Vec<ComicPlanDialogueIntent>, String> {
    let mut panels = page
        .get("panels")
        .and_then(Value::as_array)
        .ok_or_else(|| "COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".to_string())?
        .iter()
        .collect::<Vec<_>>();
    panels.sort_by_key(|panel| panel.get("panelNo").and_then(Value::as_i64).unwrap_or(0));
    let mut dialogues = Vec::new();
    for panel in panels {
        let panel_no = panel
            .get("panelNo")
            .and_then(Value::as_i64)
            .ok_or_else(|| "COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".to_string())?;
        let Some(items) = panel.get("dialogues") else {
            continue;
        };
        let items = items
            .as_array()
            .ok_or_else(|| "COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".to_string())?;
        for item in items {
            // The frozen contract allows `oneOf: [string, {text, speaker?}]` and
            // the renderer accepts both shapes, so normalize instead of failing
            // the whole analysis round on a schema-valid answer.
            let (speaker, text) = match item {
                Value::String(text) => (String::new(), text.clone()),
                Value::Object(_) => (
                    item.get("speaker")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    item.get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".to_string())?
                        .to_owned(),
                ),
                _ => return Err("COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".into()),
            };
            dialogues.push(ComicPlanDialogueIntent {
                panel_no,
                speaker,
                text,
            });
        }
    }
    Ok(dialogues)
}

fn bounds_for_panel(layout: &Value, panel_no: i64) -> Option<(f64, f64, f64, f64)> {
    let panel = layout
        .get("geometry")?
        .get("panels")?
        .as_array()?
        .iter()
        .find(|panel| panel.get("panelNo").and_then(Value::as_i64) == Some(panel_no))?;
    let bounds = panel.get("bounds")?;
    Some((
        number(bounds.get("x"))?,
        number(bounds.get("y"))?,
        number(bounds.get("width"))?,
        number(bounds.get("height"))?,
    ))
}

fn top_divider_is_slanted(layout: &Value, panel_no: i64, use_right_edge: bool) -> bool {
    let Some(points) = layout
        .get("geometry")
        .and_then(|value| value.get("panels"))
        .and_then(Value::as_array)
        .and_then(|panels| {
            panels
                .iter()
                .find(|panel| panel.get("panelNo").and_then(Value::as_i64) == Some(panel_no))
        })
        .and_then(|panel| panel.get("polygon"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let points = points
        .iter()
        .filter_map(|point| Some((number(point.get("x"))?, number(point.get("y"))?)))
        .collect::<Vec<_>>();
    if points.len() < 3 {
        return false;
    }
    let top_y = points.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
    let bottom_y = points
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);
    let x_for_y = |y: f64| -> Option<f64> {
        let values = points
            .iter()
            .filter_map(|(x, point_y)| ((point_y - y).abs() < 1e-6).then_some(*x));
        if use_right_edge {
            values.reduce(f64::max)
        } else {
            values.reduce(f64::min)
        }
    };
    x_for_y(top_y)
        .zip(x_for_y(bottom_y))
        .is_some_and(|(top, bottom)| (top - bottom).abs() >= 0.02)
}

fn five_panel_profile_geometry_matches(layout: &Value) -> bool {
    if layout.get("layoutKind").and_then(Value::as_str) != Some("template")
        || layout.get("readingOrder") != Some(&json!([1, 2, 3, 4, 5]))
        || layout.get("dominantPanel").and_then(Value::as_i64) != Some(3)
    {
        return false;
    }
    let (Some(top_left), Some(top_right), Some(middle), Some(bottom_left), Some(bottom_right)) = (
        bounds_for_panel(layout, 1),
        bounds_for_panel(layout, 2),
        bounds_for_panel(layout, 3),
        bounds_for_panel(layout, 4),
        bounds_for_panel(layout, 5),
    ) else {
        return false;
    };
    top_left.1 <= 0.06
        && top_right.1 <= 0.06
        && top_left.0 < top_right.0
        && (top_left.2 - top_right.2).abs() >= 0.02
        && top_divider_is_slanted(layout, 1, true)
        && top_divider_is_slanted(layout, 2, false)
        && top_left.1 + top_left.3 <= middle.1
        && top_right.1 + top_right.3 <= middle.1
        && middle.0 <= 0.06
        && middle.2 >= 0.9
        && (0.24..=0.30).contains(&middle.1)
        && middle.3 >= 0.35
        && middle.1 + middle.3 <= bottom_left.1
        && middle.1 + middle.3 <= bottom_right.1
        && bottom_left.1 >= 0.66
        && bottom_right.1 >= 0.66
        && bottom_left.0 < bottom_right.0
        && (bottom_left.2 - bottom_right.2).abs() >= 0.02
}

pub(crate) fn validate_comic_plan_intent_page_plan(
    intent: &ComicPlanIntent,
    body: &Value,
) -> Result<(), String> {
    let (_, pages, _) = page_plan_shape(body)?;
    if pages.len() != intent.pages.len() {
        return Err("COMIC_PLAN_INTENT_PAGE_COUNT_MISMATCH".into());
    }
    let mut by_number = std::collections::BTreeMap::new();
    for page in &pages {
        let page_no = page
            .get("pageNo")
            .and_then(Value::as_i64)
            .ok_or_else(|| "COMIC_PLAN_INTENT_PAGE_NUMBER_INVALID".to_string())?;
        if by_number.insert(page_no, page).is_some() {
            return Err("COMIC_PLAN_INTENT_PAGE_NUMBER_INVALID".into());
        }
    }
    for (index, expected) in intent.pages.iter().enumerate() {
        let page_no = index as i64 + 1;
        let page = by_number
            .get(&page_no)
            .ok_or_else(|| "COMIC_PLAN_INTENT_PAGE_NUMBER_INVALID".to_string())?;
        let panels = page
            .get("panels")
            .and_then(Value::as_array)
            .ok_or_else(|| "COMIC_PLAN_INTENT_PANEL_COUNT_MISMATCH".to_string())?;
        if panels.len() != expected.panel_count as usize {
            return Err("COMIC_PLAN_INTENT_PANEL_COUNT_MISMATCH".into());
        }
        if let Some(profile) = expected.layout_profile.as_deref() {
            if comic_plan_profile_instruction(profile).is_none()
                || page
                    .get("layout")
                    .and_then(|layout| layout.get("templateId"))
                    .and_then(Value::as_str)
                    != Some(profile)
                || !five_panel_profile_geometry_matches(
                    page.get("layout")
                        .ok_or("COMIC_PLAN_INTENT_LAYOUT_MISMATCH")?,
                )
            {
                return Err("COMIC_PLAN_INTENT_LAYOUT_MISMATCH".into());
            }
        }
        if let Some(expected_dialogues) = &expected.dialogues {
            let actual = page_panel_dialogues(page)?;
            if actual != *expected_dialogues {
                return Err("COMIC_PLAN_INTENT_DIALOGUE_MISMATCH".into());
            }
        }
    }
    if by_number.len() != intent.pages.len() {
        return Err("COMIC_PLAN_INTENT_PAGE_NUMBER_INVALID".into());
    }
    Ok(())
}

fn adaptation_analysis_output(
    value: &Value,
    adaptation_id: &str,
    chapter_id: &str,
    comic_plan_intent: Option<&ComicPlanIntent>,
) -> Result<Vec<(String, Value)>, String> {
    let items = value
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or("模型结果缺少 artifacts 数组")?;
    if items.len() != ADAPTATION_ARTIFACT_TYPES.len() {
        return Err("模型结果必须包含四种且仅一种改编产物".into());
    }
    let mut output = Vec::new();
    for item in items {
        let kind = item
            .get("artifactType")
            .and_then(Value::as_str)
            .ok_or("产物缺少 artifactType")?;
        if !ADAPTATION_ARTIFACT_TYPES.contains(&kind)
            || output.iter().any(|(known, _)| known == kind)
        {
            return Err("模型改编产物类型不完整或重复".into());
        }
        if item.get("schemaVersion").and_then(Value::as_str) != Some("novel-analysis.v1")
            || !item.get("warnings").is_some_and(Value::is_array)
        {
            return Err("改编产物 envelope 无效".into());
        }
        let owner = item
            .get("owner")
            .and_then(Value::as_object)
            .ok_or("产物缺少 owner")?;
        let expected = if kind == "adaptation_proposal" {
            adaptation_id
        } else {
            chapter_id
        };
        if owner.get("ownerType").and_then(Value::as_str) != Some(adaptation_owner_type(kind))
            || owner.get("ownerId").and_then(Value::as_str) != Some(expected)
        {
            return Err("产物 owner 无效".into());
        }
        let content = item
            .get("content")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or("产物 content 必须是对象")?;
        validate_adaptation_analysis_content(kind, &content, chapter_id)?;
        output.push((kind.into(), content));
    }
    if let Some(intent) = comic_plan_intent {
        let page_plan = output
            .iter()
            .find(|(kind, _)| kind == "page_panel_plan")
            .map(|(_, content)| content)
            .ok_or_else(|| "COMIC_PLAN_INTENT_PAGE_PLAN_MISSING".to_string())?;
        validate_comic_plan_intent_page_plan(intent, page_plan)?;
    }
    Ok(output)
}

#[derive(Clone)]
struct AdaptationAnalysisPrompt {
    run_id: String,
    prompt_version: String,
    novel_work_id: String,
    comic_adaptation_id: String,
    comic_adaptation_chapter_id: String,
    model_id: String,
    base_canon: Value,
    base_state: Value,
    base_continuity: Value,
    working_context: Value,
    adaptation_config: Value,
    comic_plan_intent: Option<ComicPlanIntent>,
    comic_plan_intent_constraints: Option<Value>,
    source_range_options: Option<Vec<Value>>,
    inputs: Vec<Value>,
}

fn adaptation_analysis_prompt(
    conn: &Connection,
    run_id: &str,
) -> Result<AdaptationAnalysisPrompt, String> {
    let row: (String,String,String,String,String,String,String,Option<String>,Option<String>,String) = conn.query_row(
        "SELECT run.id,run.prompt_version,run.novel_work_id,run.comic_adaptation_id,run.comic_adaptation_chapter_id,run.model_id,canon.body_json,run.base_novel_state_version_id,state.body_json,continuity.body_json FROM adaptation_analysis_runs run JOIN novel_canon_versions canon ON canon.id=run.base_canon_version_id LEFT JOIN novel_state_versions state ON state.id=run.base_novel_state_version_id JOIN continuity_state_versions continuity ON continuity.id=run.base_continuity_state_version_id WHERE run.id=? AND run.status='running'",
        params![run_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)),
    ).optional().map_err(|e|e.to_string())?.ok_or_else(|| "ADAPTATION_ANALYSIS_NOT_RUNNING".to_string())?;
    let mut stmt=conn.prepare("SELECT input.artifact_type,revision.id,revision.body_json FROM adaptation_analysis_run_inputs input JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id WHERE input.adaptation_analysis_run_id=? ORDER BY input.source_order").map_err(|e|e.to_string())?;
    let inputs=stmt.query_map(params![run_id],|r|Ok(json!({"artifactType":r.get::<_,String>(0)?,"revisionId":r.get::<_,String>(1)?,"content":json_value(r.get(2)?)}))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    if inputs.len() != ORIGINAL_ARTIFACT_TYPES.len() {
        return Err("FROZEN_INPUTS_INCOMPLETE".into());
    }
    let working_context: Value = conn.query_row(
        "SELECT COALESCE(json_object('id',context.id,'branchKind',context.branch_kind,'status',context.status,'canon',json(context.resolved_working_canon_json),'state',json(context.resolved_working_state_json)),'{}') FROM adaptation_analysis_runs run LEFT JOIN novel_chapter_context_revisions context ON context.source_analysis_run_id=run.source_analysis_run_id WHERE run.id=?",
        params![run_id], |r| r.get::<_,String>(0),
    ).optional().map_err(|e|e.to_string())?.and_then(|raw|serde_json::from_str(&raw).ok()).unwrap_or_else(||json!({}));
    let initial_event: Value = conn.query_row(
        "SELECT payload_json FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=? AND seq=1",
        params![run_id], |r| r.get::<_, String>(0),
    ).optional().map_err(|e| e.to_string())?
        .map(json_value)
        .ok_or_else(|| "FROZEN_BRANCH_CONFIG_MISSING".to_string())?;
    let adaptation_config = initial_event
        .get("branchConfig")
        .cloned()
        .filter(Value::is_object)
        .ok_or_else(|| "FROZEN_BRANCH_CONFIG_MISSING".to_string())?;
    let comic_plan_intent = match initial_event.get("comicPlanIntent") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            canonicalize_comic_plan_intent(Some(
                serde_json::from_value(value.clone())
                    .map_err(|_| "FROZEN_COMIC_PLAN_INTENT_INVALID".to_string())?,
            ))?
            .ok_or_else(|| "FROZEN_COMIC_PLAN_INTENT_INVALID".to_string())?,
        ),
    };
    let comic_plan_intent_constraints = match comic_plan_intent.as_ref() {
        Some(intent) => {
            let stored = initial_event
                .get("comicPlanIntentConstraints")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| "FROZEN_COMIC_PLAN_INTENT_INVALID".to_string())?;
            let canonical = comic_plan_intent_constraints(intent);
            if stored != canonical {
                return Err("FROZEN_COMIC_PLAN_INTENT_CONSTRAINTS_MISMATCH".into());
            }
            Some(canonical)
        }
        None => None,
    };
    let source_range_options = if adaptation_prompt_uses_source_range_options(&row.1) {
        let stored = initial_event
            .get("sourceRangeOptions")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?;
        let chapter_revision_id: String = conn
            .query_row(
                "SELECT chapter.novel_chapter_revision_id
                 FROM comic_adaptation_chapters chapter
                 JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id
                 WHERE chapter.id=? AND adaptation.id=? AND adaptation.novel_work_id=?",
                params![row.4.as_str(), row.3.as_str(), row.2.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?
            .ok_or("FROZEN_SOURCE_RANGE_OPTIONS_INVALID")?;
        let frozen_input_revision_ids = inputs
            .iter()
            .filter_map(|input| input.get("revisionId").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let canonical = frozen_source_range_options(
            conn,
            &row.2,
            &chapter_revision_id,
            &frozen_input_revision_ids,
        )?;
        if stored != canonical {
            return Err("FROZEN_SOURCE_RANGE_OPTIONS_MISMATCH".into());
        }
        Some(canonical)
    } else {
        None
    };
    if row.1 == ADAPTATION_ANALYSIS_PROMPT_VERSION {
        let stored = initial_event
            .get("outputIdentityBindings")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or("FROZEN_OUTPUT_IDENTITY_BINDINGS_INVALID")?;
        let canonical = adaptation_output_identity_bindings(&row.3, &row.4);
        if stored != canonical {
            return Err("FROZEN_OUTPUT_IDENTITY_BINDINGS_MISMATCH".into());
        }
    }
    let result = AdaptationAnalysisPrompt {
        run_id: row.0,
        prompt_version: row.1,
        novel_work_id: row.2,
        comic_adaptation_id: row.3,
        comic_adaptation_chapter_id: row.4,
        model_id: row.5,
        base_canon: json_value(row.6),
        base_state: row.8.map(json_value).unwrap_or_else(|| json!({})),
        base_continuity: json_value(row.9),
        working_context,
        adaptation_config,
        comic_plan_intent,
        comic_plan_intent_constraints,
        source_range_options,
        inputs,
    };
    let bytes = result
        .inputs
        .iter()
        .map(|v| v.to_string().len())
        .sum::<usize>()
        + result.base_canon.to_string().len()
        + result.base_state.to_string().len()
        + result.base_continuity.to_string().len()
        + result.working_context.to_string().len()
        + result.adaptation_config.to_string().len()
        + result
            .comic_plan_intent_constraints
            .as_ref()
            .map_or(0, |value| value.to_string().len())
        + result
            .source_range_options
            .as_ref()
            .map_or(0, |values| Value::Array(values.clone()).to_string().len())
        + (result.prompt_version == ADAPTATION_ANALYSIS_PROMPT_VERSION)
            .then(|| {
                adaptation_output_identity_bindings(
                    &result.comic_adaptation_id,
                    &result.comic_adaptation_chapter_id,
                )
                .to_string()
                .len()
            })
            .unwrap_or(0)
        + result.comic_plan_intent.as_ref().map_or(0, |value| {
            serde_json::to_string(value).map_or(usize::MAX, |raw| raw.len())
        });
    if bytes > 256 * 1024 {
        return Err("冻结分析输入超过上限".into());
    }
    Ok(result)
}

async fn request_adaptation_analysis_llm(
    app: &AppState,
    prompt: &AdaptationAnalysisPrompt,
) -> Result<Vec<(String, Value)>, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    if cfg.llm_api_url.trim().is_empty() || cfg.llm_api_key.trim().is_empty() {
        return Err("NOT_CONFIGURED".into());
    }
    let system = adaptation_analysis_system_prompt_for_frozen_constraints(
        &prompt.prompt_version,
        prompt.comic_plan_intent_constraints.as_ref(),
    )?;
    let user = adaptation_analysis_user_payload(prompt).to_string();
    let raw = crate::llm::complete_text(
        &adaptation_completion_endpoint(&cfg.llm_api_url),
        &cfg.llm_api_key,
        &prompt.model_id,
        &system,
        &user,
        "novel.adaptation_analysis",
    )
    .await
    .map_err(|e| adaptation_safe_error(&e))?;
    adaptation_analysis_output(
        &parse_adaptation_model_json(&raw)?,
        &prompt.comic_adaptation_id,
        &prompt.comic_adaptation_chapter_id,
        prompt.comic_plan_intent.as_ref(),
    )
}

fn adaptation_analysis_user_payload(prompt: &AdaptationAnalysisPrompt) -> Value {
    let mut user = json!({"runId":prompt.run_id,"novelWorkId":prompt.novel_work_id,"frozenOriginalArtifacts":prompt.inputs,"baseCanon":prompt.base_canon,"baseNovelState":prompt.base_state,"baseContinuity":prompt.base_continuity,"workingContext":prompt.working_context,"adaptationConfig":prompt.adaptation_config,"comicPlanIntent":prompt.comic_plan_intent,"comicPlanIntentConstraints":prompt.comic_plan_intent_constraints,"ownerMap":{"comic_adaptation":prompt.comic_adaptation_id,"comic_chapter":prompt.comic_adaptation_chapter_id},"instruction":"仅输出 JSON。"});
    if let Some(source_range_options) = &prompt.source_range_options {
        user["sourceRangeOptions"] = Value::Array(source_range_options.clone());
    }
    if prompt.prompt_version == ADAPTATION_ANALYSIS_PROMPT_VERSION {
        user["outputIdentityBindings"] = adaptation_output_identity_bindings(
            &prompt.comic_adaptation_id,
            &prompt.comic_adaptation_chapter_id,
        );
    }
    user
}

fn is_safe_layout_diagnostic(error: &str) -> bool {
    let Some((reason, path)) = error
        .strip_prefix("LAYOUT_INVALID:")
        .and_then(|value| value.split_once(':'))
    else {
        return false;
    };
    if !matches!(
        reason,
        "CONTRACT"
            | "PANEL_COUNT"
            | "READING_ORDER"
            | "PANEL_IDENTITY"
            | "TEMPLATE"
            | "GEOMETRY"
            | "GUTTER"
            | "SAFE_AREA"
            | "POLYGON"
            | "POLYGON_TOPOLOGY"
            | "PANEL_SIZE"
            | "BOUNDS"
            | "TEXT_ZONE"
            | "TEXT_ZONE_POLYGON"
            | "BLEED"
            | "PARENT"
            | "PARENT_CYCLE"
            | "OVERLAP_OR_GUTTER"
    ) {
        return false;
    }
    let Some(rest) = path.strip_prefix("pages[") else {
        return false;
    };
    let Some((page_index, rest)) = rest.split_once("].layout.") else {
        return false;
    };
    if page_index.is_empty() || !page_index.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if matches!(
        rest,
        "contract"
            | "panelCount"
            | "readingOrder"
            | "panels"
            | "templateId"
            | "geometry"
            | "geometry.gutter"
            | "geometry.safeArea"
            | "geometry.panels"
    ) {
        return true;
    }
    let Some(panel) = rest.strip_prefix("geometry.panels[") else {
        return false;
    };
    let Some((panel_index, field)) = panel.split_once("].") else {
        return false;
    };
    !panel_index.is_empty()
        && panel_index.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(
            field,
            "panelNo" | "polygon" | "bounds" | "textZone" | "bleed" | "parentPanelNo"
        )
}

fn adaptation_analysis_failure_fields(error: String) -> (&'static str, String) {
    if error == "NOT_CONFIGURED" {
        ("NOT_CONFIGURED", error)
    } else if error == "EVIDENCE_RANGE_INVALID" {
        (
            "EVIDENCE_RANGE_INVALID",
            "改编引用不是有效的 UTF-8 字节范围，未生成漫画页。".into(),
        )
    } else if error == "EVIDENCE_SOURCE_SCOPE_INVALID" {
        (
            "EVIDENCE_SOURCE_SCOPE_INVALID",
            "改编引用不属于当前小说，未生成漫画页。".into(),
        )
    } else if error == "EVIDENCE_SOURCE_READ_FAILED" {
        (
            "EVIDENCE_SOURCE_READ_FAILED",
            "无法读取改编引用的冻结正文，未生成漫画页。".into(),
        )
    } else if error == "EVIDENCE_RANGE_OPTION_MISMATCH" {
        (
            "EVIDENCE_RANGE_OPTION_MISMATCH",
            "改编引用未使用本次冻结的来源范围。".into(),
        )
    } else if is_safe_layout_diagnostic(&error) {
        (
            "LAYOUT_INVALID",
            format!("漫画页面布局未通过本地几何校验：{error}"),
        )
    } else if error.starts_with("LAYOUT_INVALID:") {
        (
            "ANALYSIS_FAILED",
            "改编分析返回了无法识别的布局错误，未开始生成漫画页。".into(),
        )
    } else if error.starts_with("COMIC_PLAN_INTENT_") {
        (
            "COMIC_PLAN_INTENT_MISMATCH",
            "改编规划未满足本次已保存的漫画页面要求，未开始生成漫画页。".into(),
        )
    } else {
        ("ANALYSIS_FAILED", error)
    }
}

fn adaptation_run_after_tx(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<AdaptationAnalysisRun, String> {
    tx.query_row(
        &format!("{ADAPTATION_RUN_SELECT} WHERE id=?"),
        params![id],
        adaptation_analysis_from_row,
    )
    .map_err(|e| e.to_string())
}

fn fail_adaptation_analysis_inner(
    conn: &Connection,
    run_id: &str,
    owner: &str,
    code: &str,
    message: &str,
) -> Result<AdaptationAnalysisRun, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let ts = now();
    let message = adaptation_safe_error(message);
    let changed=tx.execute("UPDATE adaptation_analysis_run_attempts SET status='error',safe_error_code=?,safe_user_message=?,finished_at=? WHERE adaptation_analysis_run_id=? AND attempt_no=(SELECT attempt_no FROM adaptation_analysis_runs WHERE id=?) AND status='running' AND lease_owner=? AND lease_expires_at>=?",params![code,message,ts,run_id,run_id,owner,ts]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("ADAPTATION_ANALYSIS_LEASE_LOST".into());
    }
    let changed=tx.execute("UPDATE adaptation_analysis_runs SET status='error',lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=NULL,safe_error_code=?,safe_user_message=?,updated_at=?,finished_at=? WHERE id=? AND status='running'",params![code,message,ts,ts,run_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("ADAPTATION_ANALYSIS_STATE_CONFLICT".into());
    }
    tx.execute("INSERT INTO adaptation_analysis_run_events(id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at) VALUES (?,?,(SELECT COALESCE(MAX(seq),0)+1 FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=?),'error',?,?)",params![new_id("adaptevent"),run_id,run_id,json!({"code":code}).to_string(),ts]).map_err(|e|e.to_string())?;
    let run = adaptation_run_after_tx(&tx, run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run)
}

pub(crate) fn complete_adaptation_analysis_inner(
    conn: &Connection,
    run_id: &str,
    artifacts: Vec<(String, Value)>,
    owner: &str,
) -> Result<AdaptationAnalysisRun, String> {
    if artifacts.len() != ADAPTATION_ARTIFACT_TYPES.len() {
        return Err("ADAPTATION_OUTPUT_INCOMPLETE".into());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let run = adaptation_run_after_tx(&tx, run_id)?;
    let owns:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM adaptation_analysis_run_attempts WHERE adaptation_analysis_run_id=? AND attempt_no=? AND status='running' AND lease_owner=? AND lease_expires_at>=?)",params![run_id,run.attempt_no,owner,now()],|r|r.get(0)).map_err(|e|e.to_string())?;
    if run.status != "running" || !owns {
        return Err("ADAPTATION_ANALYSIS_LEASE_LOST".into());
    }
    let chapter_revision: String = tx
        .query_row(
            "SELECT novel_chapter_revision_id FROM comic_adaptation_chapters WHERE id=?",
            params![run.comic_adaptation_chapter_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let chapter_plan = artifacts
        .iter()
        .find_map(|(kind, content)| (kind == "comic_chapter_plan").then_some(content))
        .ok_or("ADAPTATION_OUTPUT_INCOMPLETE")?;
    validate_comic_chapter_plan_evidence_ranges(&tx, &run.novel_work_id, chapter_plan)?;
    validate_v4_comic_chapter_plan_source_options(&tx, &run, chapter_plan)?;
    let ts = now();
    for (kind, content) in artifacts {
        let artifact_id = new_id("adaptartifact");
        let revision_id = new_id("adaptartifactrev");
        let chapter_scope =
            (kind != "adaptation_proposal").then(|| run.comic_adaptation_chapter_id.clone());
        tx.execute("INSERT INTO analysis_artifacts(id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES (?,NULL,?,?,?,?,?,?,?,NULL,'active',0,?,?)",params![artifact_id,run.id,kind,run.novel_work_id,chapter_revision,run.comic_adaptation_id,chapter_scope,revision_id,ts,ts]).map_err(|e|format!("保存改编产物失败: {e}"))?;
        tx.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,1,NULL,?,'','ai_adaptation_analysis','{}','{}','candidate',?)",params![revision_id,artifact_id,content.to_string(),ts]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO adaptation_analysis_run_artifacts(adaptation_analysis_run_id,artifact_type,analysis_artifact_id) VALUES (?,?,?)",params![run.id,kind,artifact_id]).map_err(|e|e.to_string())?;
    }
    let changed=tx.execute("UPDATE adaptation_analysis_run_attempts SET status='success',finished_at=? WHERE adaptation_analysis_run_id=? AND attempt_no=? AND status='running' AND lease_owner=?",params![ts,run.id,run.attempt_no,owner]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("ADAPTATION_ANALYSIS_LEASE_LOST".into());
    }
    let changed=tx.execute("UPDATE adaptation_analysis_runs SET status='ready_for_review',lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=NULL,updated_at=?,finished_at=? WHERE id=? AND status='running'",params![ts,ts,run.id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("ADAPTATION_ANALYSIS_STATE_CONFLICT".into());
    }
    tx.execute("INSERT INTO adaptation_analysis_run_events(id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at) VALUES (?,?,(SELECT COALESCE(MAX(seq),0)+1 FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=?),'completed','{}',?)",params![new_id("adaptevent"),run.id,run.id,ts]).map_err(|e|e.to_string())?;
    let result = adaptation_run_after_tx(&tx, &run.id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

fn finish_adaptation_analysis_dispatch(
    conn: &Connection,
    run_id: &str,
    owner: &str,
    result: Result<Vec<(String, Value)>, String>,
) -> Result<AdaptationAnalysisRun, String> {
    match result {
        Ok(artifacts) => match complete_adaptation_analysis_inner(conn, run_id, artifacts, owner) {
            Ok(run) => Ok(run),
            Err(error) => {
                let (code, message) = adaptation_analysis_failure_fields(error);
                fail_adaptation_analysis_inner(conn, run_id, owner, code, &message)
            }
        },
        Err(error) => {
            let (code, message) = adaptation_analysis_failure_fields(error);
            fail_adaptation_analysis_inner(conn, run_id, owner, code, &message)
        }
    }
}

async fn dispatch_adaptation_analysis(
    state: &tauri::State<'_, DbState>,
    app: &tauri::State<'_, AppState>,
    run_id: &str,
    owner: &str,
) -> Result<AdaptationAnalysisRun, String> {
    let result = match db::with_connection(state, |conn| adaptation_analysis_prompt(conn, run_id)) {
        Ok(prompt) => request_adaptation_analysis_llm(app, &prompt).await,
        Err(error) => Err(error),
    };
    db::with_connection(state, |conn| {
        finish_adaptation_analysis_dispatch(conn, run_id, owner, result)
    })
}

#[tauri::command]
pub async fn novel_adaptation_analysis_start(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    input: AdaptationAnalysisStartInput,
) -> Result<AdaptationAnalysisRun, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    let configured = !cfg.llm_api_url.trim().is_empty() && !cfg.llm_api_key.trim().is_empty();
    let mut provider = input
        .provider_id
        .clone()
        .unwrap_or_else(|| "configured_llm".into());
    let mut model = input
        .model_id
        .clone()
        .unwrap_or_else(|| cfg.llm_model.clone());
    if provider.trim().eq_ignore_ascii_case("default") {
        provider = "configured_llm".into();
    }
    if model.trim().eq_ignore_ascii_case("default") {
        model = cfg.llm_model.clone();
    }
    let owner = state.app_session_id().to_owned();
    let record = db::with_connection(&state, |conn| {
        start_adaptation_analysis_inner(conn, input, configured, provider, model, &owner)
    })?;
    if !record.dispatch {
        return Ok(record.run);
    }
    dispatch_adaptation_analysis(&state, &app, &record.run.id, &owner).await
}

#[tauri::command]
pub fn novel_adaptation_analysis_status(
    state: tauri::State<'_, DbState>,
    input: AdaptationAnalysisStatusInput,
) -> Result<AdaptationAnalysisRun, String> {
    db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        conn.query_row(
            &format!(
                "{ADAPTATION_RUN_SELECT} WHERE id=? AND novel_work_id=? AND comic_adaptation_id=?"
            ),
            params![
                input.adaptation_analysis_run_id,
                work.id,
                input.comic_adaptation_id
            ],
            adaptation_analysis_from_row,
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ADAPTATION_ANALYSIS_RUN_UNKNOWN".into())
    })
}

#[tauri::command]
pub fn novel_adaptation_analysis_list(
    state: tauri::State<'_, DbState>,
    input: AdaptationAnalysisListInput,
) -> Result<Vec<AdaptationAnalysisRun>, String> {
    db::with_connection(&state, |conn| adaptation_analysis_list_inner(conn, input))
}

fn adaptation_analysis_list_inner(
    conn: &Connection,
    input: AdaptationAnalysisListInput,
) -> Result<Vec<AdaptationAnalysisRun>, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let mut statement = conn
            .prepare(&format!(
                "{ADAPTATION_RUN_SELECT} WHERE novel_work_id=? AND comic_adaptation_id=? AND (? IS NULL OR comic_adaptation_chapter_id=?) AND (? IS NULL OR status=?) ORDER BY created_at,id"
            ))
            .map_err(|e| e.to_string())?;
    let runs = statement
        .query_map(
            params![
                work.id,
                input.comic_adaptation_id,
                input.comic_adaptation_chapter_id,
                input.comic_adaptation_chapter_id,
                input.status,
                input.status
            ],
            adaptation_analysis_from_row,
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(runs)
}

fn retry_adaptation_analysis_inner(
    conn: &Connection,
    input: AdaptationAnalysisRetryInput,
    configured: bool,
    owner: &str,
) -> Result<AdaptationAnalysisStartRecord, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    let key = input.idempotency_key.clone();
    with_receipt(
        conn,
        "novel_adaptation_analysis_retry",
        &key,
        &request,
        move |tx| {
            active_work_in_tx(tx, &input.project_id, &work.id)?;
            let run:AdaptationAnalysisRun=tx.query_row(&format!("{ADAPTATION_RUN_SELECT} WHERE id=? AND novel_work_id=? AND comic_adaptation_id=? AND status IN ('error','stale','cancelled','unknown_manual')"),params![input.adaptation_analysis_run_id,work.id,input.comic_adaptation_id],adaptation_analysis_from_row).optional().map_err(|e|e.to_string())?.ok_or_else(||"只有 error、stale、cancelled 或 unknown_manual run 可以重试".to_string())?;
            let inputs:i64=tx.query_row("SELECT COUNT(*) FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=?",params![run.id],|r|r.get(0)).map_err(|e|e.to_string())?;
            if inputs != 10 {
                return Err("FROZEN_INPUTS_INCOMPLETE".into());
            }
            let ts = now();
            let next = run.attempt_no + 1;
            let parent:String=tx.query_row("SELECT id FROM adaptation_analysis_run_attempts WHERE adaptation_analysis_run_id=? AND attempt_no=?",params![run.id,run.attempt_no],|r|r.get(0)).map_err(|e|e.to_string())?;
            let changed=tx.execute("UPDATE adaptation_analysis_runs SET attempt_no=?,updated_at=? WHERE id=? AND attempt_no=? AND status IN ('error','stale','cancelled','unknown_manual')",params![next,ts,run.id,run.attempt_no]).map_err(|e|e.to_string())?;
            if changed != 1 {
                return Err("ADAPTATION_ANALYSIS_STATE_CONFLICT".into());
            }
            let attempt = new_id("adaptattempt");
            let expiry = ts + ADAPTATION_ANALYSIS_LEASE_MS;
            tx.execute("INSERT INTO adaptation_analysis_run_attempts(id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,created_at) VALUES (?,?,?,?, 'queued',?,?,?,?)",params![attempt,run.id,next,parent,owner,expiry,ts,ts]).map_err(|e|e.to_string())?;
            tx.execute("UPDATE adaptation_analysis_runs SET status='queued',lease_owner=?,lease_expires_at=?,heartbeat_at=?,safe_error_code=NULL,safe_user_message=NULL,updated_at=?,finished_at=NULL WHERE id=? AND status IN ('error','stale','cancelled','unknown_manual')",params![owner,expiry,ts,ts,run.id]).map_err(|e|e.to_string())?;
            if configured {
                tx.execute(
                    "UPDATE adaptation_analysis_run_attempts SET status='running' WHERE id=?",
                    params![attempt],
                )
                .map_err(|e| e.to_string())?;
                let changed=tx.execute("UPDATE adaptation_analysis_runs SET status='running',updated_at=? WHERE id=? AND status='queued'",params![ts,run.id]).map_err(|e|e.to_string())?;
                if changed != 1 {
                    return Err("ADAPTATION_ANALYSIS_STATE_CONFLICT".into());
                }
            } else {
                tx.execute("UPDATE adaptation_analysis_run_attempts SET status='error',lease_owner=NULL,lease_expires_at=NULL,safe_error_code='NOT_CONFIGURED',safe_user_message='未配置 LLM',finished_at=? WHERE id=?",params![ts,attempt]).map_err(|e|e.to_string())?;
                tx.execute("UPDATE adaptation_analysis_runs SET status='error',lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=NULL,safe_error_code='NOT_CONFIGURED',safe_user_message='未配置 LLM，可配置后重试',updated_at=?,finished_at=? WHERE id=? AND status='queued'",params![ts,ts,run.id]).map_err(|e|e.to_string())?;
            }
            tx.execute("INSERT INTO adaptation_analysis_run_events(id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at) VALUES (?,?,(SELECT COALESCE(MAX(seq),0)+1 FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=?),'retried',?,?)",params![new_id("adaptevent"),run.id,run.id,json!({"attemptNo":next}).to_string(),ts]).map_err(|e|e.to_string())?;
            let result = adaptation_run_after_tx(tx, &run.id)?;
            Ok(AdaptationAnalysisStartRecord {
                run: result,
                dispatch: configured,
            })
        },
    )
}

#[tauri::command]
pub async fn novel_adaptation_analysis_retry(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    input: AdaptationAnalysisRetryInput,
) -> Result<AdaptationAnalysisRun, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    let configured = !cfg.llm_api_url.trim().is_empty() && !cfg.llm_api_key.trim().is_empty();
    let owner = state.app_session_id().to_owned();
    let record = db::with_connection(&state, |conn| {
        retry_adaptation_analysis_inner(conn, input, configured, &owner)
    })?;
    if !record.dispatch {
        return Ok(record.run);
    }
    dispatch_adaptation_analysis(&state, &app, &record.run.id, &owner).await
}

#[tauri::command]
pub fn novel_adaptation_analysis_recover_stale(
    state: tauri::State<'_, DbState>,
    input: AdaptationAnalysisRecoverInput,
) -> Result<i64, String> {
    db::with_connection(&state, |conn| {
        novel_adaptation_analysis_recover_stale_inner(
            conn,
            &input.project_id,
            &input.novel_work_id,
            &input.comic_adaptation_id,
        )
    })
}

fn novel_adaptation_analysis_recover_stale_inner(
    conn: &Connection,
    project_id: &str,
    novel_work_id: &str,
    adaptation_id: &str,
) -> Result<i64, String> {
    let work = get_work(conn, project_id, novel_work_id)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let ts = now();
    let changed=tx.execute("UPDATE adaptation_analysis_run_attempts SET status='stale',safe_error_code='LEASE_EXPIRED',safe_user_message='改编分析执行租约已过期，可重试',finished_at=? WHERE status='running' AND lease_expires_at<? AND adaptation_analysis_run_id IN (SELECT id FROM adaptation_analysis_runs WHERE novel_work_id=? AND comic_adaptation_id=? AND status='running')",params![ts,ts,work.id,adaptation_id]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE adaptation_analysis_runs SET status='stale',lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=NULL,safe_error_code='LEASE_EXPIRED',safe_user_message='改编分析执行租约已过期，可重试',updated_at=?,finished_at=? WHERE novel_work_id=? AND comic_adaptation_id=? AND status='running' AND EXISTS(SELECT 1 FROM adaptation_analysis_run_attempts attempt WHERE attempt.adaptation_analysis_run_id=adaptation_analysis_runs.id AND attempt.attempt_no=adaptation_analysis_runs.attempt_no AND attempt.status='stale')",params![ts,ts,work.id,adaptation_id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed as i64)
}

fn active_plan_head(
    tx: &Transaction<'_>,
    adaptation_id: &str,
    id: &str,
) -> Result<(String, String, String), String> {
    tx.query_row(
        "SELECT adaptation_proposal_revision_id,scene_plan_revision_id,comic_chapter_plan_revision_id FROM comic_adaptation_plan_heads WHERE id=? AND comic_adaptation_id=? AND status='active'",
        params![id, adaptation_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional().map_err(|e|e.to_string())?.ok_or_else(|| "PLAN_HEAD_STALE".into())
}

fn planning_chapter_for_page_plan(
    tx: &Transaction<'_>,
    adaptation_id: &str,
    plan_head_id: &str,
    page_plan_revision_id: &str,
    comic_chapter_draft_id: &str,
) -> Result<(String, String), String> {
    let scope: Option<String> = tx
        .query_row(
            "SELECT comic_chapter_id FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE revision.id=? AND artifact.comic_adaptation_id=?",
            params![page_plan_revision_id, adaptation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if scope.as_deref() != Some(comic_chapter_draft_id) {
        return Err("COMIC_CHAPTER_SCOPE_INVALID".into());
    }
    let mut statement = tx.prepare("SELECT planning.id,planning.planning_chapter_stable_key FROM comic_planning_chapters planning JOIN comic_planning_chapter_sources source ON source.comic_planning_chapter_id=planning.id JOIN comic_adaptation_chapters chapter ON chapter.novel_chapter_revision_id=source.novel_chapter_revision_id WHERE planning.comic_adaptation_plan_head_id=? AND planning.comic_adaptation_id=? AND planning.status='planning' AND chapter.id=? AND chapter.comic_adaptation_id=? ORDER BY planning.planning_chapter_stable_key").map_err(|e|e.to_string())?;
    let rows = statement
        .query_map(
            params![
                plan_head_id,
                adaptation_id,
                comic_chapter_draft_id,
                adaptation_id
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    match rows.as_slice() {
        [row] => Ok(row.clone()),
        _ => Err("PLANNING_CHAPTER_AMBIGUOUS".into()),
    }
}

#[tauri::command]
pub fn novel_adaptation_accept_preview(
    state: tauri::State<'_, DbState>,
    input: AdaptationAcceptPreviewInput,
) -> Result<ApplyPreview, String> {
    db::with_connection(&state, |conn| accept_preview_inner(conn, input))
}
fn accept_preview_inner(
    conn: &Connection,
    input: AdaptationAcceptPreviewInput,
) -> Result<ApplyPreview, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let owns:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM comic_adaptations WHERE id=? AND novel_work_id=? AND project_id=? AND status='active')",params![input.comic_adaptation_id,work.id,input.project_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !owns {
        return Err("OWNER_MISMATCH".into());
    }
    let (continuity, optimistic): (Option<String>, i64) = tx
        .query_row(
            "SELECT current_continuity_version_id,optimistic_version FROM comic_adaptations WHERE id=?",
            params![input.comic_adaptation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let continuity = continuity.ok_or("CONTINUITY_BASELINE_REQUIRED")?;
    let canon = work.published_canon_version_id.clone();
    if input
        .base_canon_version_id
        .as_deref()
        .is_some_and(|id| id != canon)
        || input
            .base_continuity_version_id
            .as_deref()
            .is_some_and(|id| id != continuity)
        || input
            .expected_adaptation_version
            .is_some_and(|v| v != optimistic)
    {
        return Err("BASELINE_STALE".into());
    }
    let _proposal = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.adaptation_proposal_revision_id,
        "adaptation_proposal",
    )?;
    let chapter = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.comic_chapter_plan_revision_id,
        "comic_chapter_plan",
    )?;
    let _scene = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.scene_plan_revision_id,
        "scene_plan",
    )?;
    validate_comic_chapter_plan_evidence_ranges(&tx, &work.id, &chapter)?;
    let chapters = chapter_keys_and_ranges(&chapter)?;
    let keys = chapters.iter().map(|v| v.0.clone()).collect::<Vec<_>>();
    let planning_chapter_summaries = chapters
        .iter()
        .map(|(key, ranges)| PlanningChapterSummary {
            planning_chapter_stable_key: key.clone(),
            source_revision_ids: ranges.iter().map(|range| range.0.clone()).collect(),
            source_ranges: ranges
                .iter()
                .map(|range| PlanningSourceRangeDto {
                    novel_chapter_revision_id: range.0.clone(),
                    start_utf8_byte: range.1,
                    end_utf8_byte: range.2,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let fp = request_hash(
        &json!({"type":"accept_adaptation","adaptation":input.comic_adaptation_id,"canon":canon,"continuity":continuity,"expectedAdaptationVersion":optimistic,"sources":[input.adaptation_proposal_revision_id,input.comic_chapter_plan_revision_id,input.scene_plan_revision_id],"chapters":chapters}),
    )?;
    let existing:Option<(String,String,String)>=tx.query_row("SELECT id,preview_fingerprint,status FROM analysis_apply_operations WHERE idempotency_key=?",params![input.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
    let id = if let Some((id, old, status)) = existing {
        if old != fp || status != "previewed" {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        id
    } else {
        let id = new_id("apply");
        tx.execute("INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,expected_adaptation_version,idempotency_key,preview_fingerprint,approval_token_hash,approval_expires_at,status,created_at,updated_at) VALUES (?,'accept_adaptation',NULL,?,?,?,?,?,?,?, 'previewed',?,?)",params![id,input.comic_adaptation_id,Option::<String>::None,optimistic,input.idempotency_key,fp,"pending",0_i64,now(),now()]).map_err(|e|e.to_string())?;
        for (n, revision) in [
            input.adaptation_proposal_revision_id.clone(),
            input.comic_chapter_plan_revision_id.clone(),
            input.scene_plan_revision_id.clone(),
        ]
        .iter()
        .enumerate()
        {
            tx.execute("INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,'adaptation_input',?)",params![id,revision,n as i64]).map_err(|e|e.to_string())?;
        }
        id
    };
    let token = new_id("approval");
    let expires = now() + TOKEN_TTL_MS;
    tx.execute("UPDATE analysis_apply_operations SET approval_token_hash=?,approval_expires_at=?,updated_at=? WHERE id=?",params![request_hash(&json!(token))?,expires,now(),id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ApplyPreview {
        operation_id: id,
        comic_adaptation_id: input.comic_adaptation_id,
        approval_token: token,
        preview_fingerprint: fp,
        base_canon_version_id: canon,
        base_continuity_version_id: continuity,
        expected_adaptation_version: optimistic,
        expires_at: expires,
        planning_chapter_keys: keys,
        planning_chapter_summaries,
    })
}

#[tauri::command]
pub fn novel_adaptation_accept(
    state: tauri::State<'_, DbState>,
    input: AdaptationAcceptInput,
) -> Result<ApplyResult, String> {
    db::with_connection(&state, |conn| accept_inner(conn, input))
}

fn accept_inner(conn: &Connection, input: AdaptationAcceptInput) -> Result<ApplyResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let op:Option<(String,String,i64,String,String,i64)>=tx.query_row("SELECT comic_adaptation_id,status,approval_expires_at,approval_token_hash,idempotency_key,expected_adaptation_version FROM analysis_apply_operations WHERE id=? AND operation_type='accept_adaptation'",params![input.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|e|e.to_string())?;
    let (adaptation, status, expires, hash, idempotency_key, expected_version) =
        op.ok_or("OPERATION_UNKNOWN")?;
    if adaptation != input.comic_adaptation_id {
        return Err("OWNER_MISMATCH".into());
    }
    if idempotency_key != input.idempotency_key {
        return Err("IDEMPOTENCY_MISMATCH".into());
    }
    if status == "succeeded" {
        return Ok(ApplyResult {
            operation_id: input.operation_id.clone(),
            status,
            entity_map: receipt_entity_map(&tx, &input.operation_id)?,
            receipt_id: Some(input.operation_id),
            safe_error: None,
        });
    }
    if status != "previewed"
        || expires < now()
        || hash != request_hash(&json!(input.approval_token))?
    {
        return Err("APPROVAL_EXPIRED".into());
    }
    let ids = {
        let mut stmt = tx
                .prepare("SELECT analysis_artifact_revision_id FROM analysis_apply_operation_sources WHERE analysis_apply_operation_id=? ORDER BY source_order")
                .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![input.operation_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    if ids.len() != 3 {
        return Err("REVISION_CONFLICT".into());
    };
    let _proposal = adopted(&tx, &adaptation, &ids[0], "adaptation_proposal")?;
    let chapter = adopted(&tx, &adaptation, &ids[1], "comic_chapter_plan")?;
    let _scene = adopted(&tx, &adaptation, &ids[2], "scene_plan")?;
    validate_comic_chapter_plan_evidence_ranges(&tx, &work.id, &chapter)?;
    let plans = chapter_keys_and_ranges(&chapter)?;
    let ts = now();
    let changed = tx.execute("UPDATE comic_adaptations SET optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND optimistic_version=?",params![ts,adaptation,expected_version]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("REVISION_CONFLICT".into());
    }
    tx.execute("UPDATE comic_adaptation_plan_heads SET status='superseded',updated_at=? WHERE comic_adaptation_id=? AND status='active'",params![ts,adaptation]).map_err(|e|e.to_string())?;
    let head = new_id("aplan");
    tx.execute("INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES (?,?,?,?,?,?,'active',?,?)",params![head,adaptation,ids[0],ids[1],ids[2],input.operation_id,ts,ts]).map_err(|e|e.to_string())?;
    tx.execute("INSERT INTO analysis_apply_receipts (analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES (?,?,'accept_adaptation',NULL,NULL,?)",params![input.operation_id,input.idempotency_key,ts]).map_err(|e|e.to_string())?;
    let mut map = Vec::new();
    for (n, (key, ranges)) in plans.iter().enumerate() {
        let id = new_id("pchapter");
        tx.execute("INSERT INTO comic_planning_chapters (id,comic_adaptation_plan_head_id,comic_adaptation_id,planning_chapter_stable_key,status,created_at,updated_at) VALUES (?,?,?,?,'planning',?,?)",params![id,head,adaptation,key,ts,ts]).map_err(|e|e.to_string())?;
        for (o, (revision, start, end)) in ranges.iter().enumerate() {
            tx.execute("INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES (?,?,?,?,?)",params![id,revision,o as i64,start,end]).map_err(|_|"EVIDENCE_RANGE_INVALID".to_string())?;
        }
        tx.execute("INSERT INTO analysis_apply_receipt_entity_maps (analysis_apply_operation_id,entity_kind,stable_key,entity_id,created_at) VALUES (?,'planning_chapter',?,?,?)",params![input.operation_id,key,id,ts]).map_err(|e|e.to_string())?;
        map.push(ApplyEntityMap {
            entity_kind: "planning_chapter".into(),
            stable_key: key.clone(),
            entity_id: id,
        });
        let _ = n;
    }
    tx.execute("UPDATE analysis_apply_operations SET status='succeeded',completed_at=?,updated_at=? WHERE id=? AND status='previewed'",params![ts,ts,input.operation_id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ApplyResult {
        operation_id: input.operation_id.clone(),
        status: "succeeded".into(),
        receipt_id: Some(input.operation_id),
        entity_map: map,
        safe_error: None,
    })
}

#[tauri::command]
pub fn novel_scene_context_resolve(
    state: tauri::State<'_, DbState>,
    input: SceneContextResolveInput,
) -> Result<SceneContextSnapshot, String> {
    db::with_connection(&state, |conn| scene_context_resolve_inner(conn, input))
}

fn scene_context_resolve_inner(
    conn: &Connection,
    input: SceneContextResolveInput,
) -> Result<SceneContextSnapshot, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let request = request_hash(&json!({
        "projectId":input.project_id,"novelWorkId":input.novel_work_id,"comicAdaptationId":input.comic_adaptation_id,
        "scenePlanRevisionId":input.scene_plan_revision_id,"planningSceneStableKey":input.planning_scene_stable_key,
        "workingContextRevisionId":input.working_context_revision_id,"canonVersionId":input.canon_version_id,
        "novelStateVersionId":input.novel_state_version_id,"continuityVersionId":input.continuity_version_id,
        "adaptationPlanRevisionId":input.adaptation_plan_revision_id,"selectedEntityIds":input.selected_entity_ids,
        "visualCardRevisionIds":input.visual_card_revision_ids,
    }))?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if let Some((stored, response)) = tx.query_row(
        "SELECT request_hash,response_json FROM novel_operation_receipts WHERE command_name='novel_scene_context_resolve' AND idempotency_key=?",
        params![input.idempotency_key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    ).optional().map_err(|e|e.to_string())? {
        if stored != request { return Err("idempotencyKey 已用于不同业务载荷".into()); }
        return serde_json::from_str(&response).map_err(|_| "RECEIPT_CORRUPT".into());
    }
    adaptation_belongs_to_work(&tx, &input.project_id, &work.id, &input.comic_adaptation_id)?;
    let scene = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.scene_plan_revision_id,
        "scene_plan",
    )?;
    let _proposal = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.adaptation_plan_revision_id,
        "adaptation_proposal",
    )?;
    let key_present = scene
        .get("scenes")
        .and_then(Value::as_array)
        .is_some_and(|scenes| {
            scenes.iter().any(|s| {
                s.get("stableKey").and_then(Value::as_str)
                    == Some(input.planning_scene_stable_key.as_str())
            })
        });
    if !key_present {
        return Err("PLANNING_SCENE_UNKNOWN".into());
    }
    let canon_ok: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM novel_canon_versions WHERE id=? AND novel_work_id=? AND status='published')",params![input.canon_version_id,work.id],|r|r.get(0)).map_err(|e|e.to_string())?;
    let state_ok: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM novel_state_versions WHERE id=? AND novel_work_id=?)",
            params![input.novel_state_version_id, work.id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !canon_ok || !state_ok {
        return Err("BASELINE_SCOPE_INVALID".into());
    }
    if let Some(context) = input.working_context_revision_id.as_deref() {
        let context_ok: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM novel_chapter_context_revisions context JOIN novel_analysis_lineages lineage ON lineage.id=context.novel_analysis_lineage_id WHERE context.id=? AND lineage.novel_work_id=?)",params![context,work.id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !context_ok {
            return Err("WORKING_CONTEXT_SCOPE_INVALID".into());
        }
    }
    let continuity = ensure_continuity_baseline(&tx, &input.comic_adaptation_id)?;
    if input
        .continuity_version_id
        .as_deref()
        .is_some_and(|id| id != continuity)
    {
        return Err("BASELINE_STALE".into());
    }
    for entity in &input.selected_entity_ids {
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM novel_entities WHERE id=? AND novel_work_id=? AND lifecycle='active')",params![entity,work.id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid {
            return Err("ENTITY_SCOPE_INVALID".into());
        }
    }
    for card in &input.visual_card_revision_ids {
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM comic_card_revisions WHERE id=? AND comic_adaptation_id=? AND status='approved')",params![card,input.comic_adaptation_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid {
            return Err("VISUAL_CARD_SCOPE_INVALID".into());
        }
    }
    let canon_body: String = tx
        .query_row(
            "SELECT body_json FROM novel_canon_versions WHERE id=?",
            params![input.canon_version_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let state_body: String = tx
        .query_row(
            "SELECT body_json FROM novel_state_versions WHERE id=?",
            params![input.novel_state_version_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let continuity_body: String = tx
        .query_row(
            "SELECT body_json FROM continuity_state_versions WHERE id=? AND comic_adaptation_id=?",
            params![continuity, input.comic_adaptation_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let working = if let Some(context_id) = input.working_context_revision_id.as_deref() {
        let (canon, state): (String, String) = tx.query_row("SELECT resolved_working_canon_json,resolved_working_state_json FROM novel_chapter_context_revisions WHERE id=?",params![context_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
        json!({"id":context_id,"canon":json_value(canon),"state":json_value(state)})
    } else {
        Value::Null
    };
    let mut frozen_entities = Vec::new();
    for entity_id in &input.selected_entity_ids {
        let frozen: (String, String, String) = tx.query_row("SELECT entity_kind,stable_key,lifecycle FROM novel_entities WHERE id=? AND novel_work_id=?",params![entity_id,work.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;
        frozen_entities.push(
            json!({"id":entity_id,"kind":frozen.0,"stableKey":frozen.1,"lifecycle":frozen.2}),
        );
    }
    let mut frozen_cards = Vec::new();
    for card_id in &input.visual_card_revision_ids {
        let frozen: (String, String, Option<String>) = tx.query_row("SELECT body_json,content_hash,asset_id FROM comic_card_revisions WHERE id=? AND comic_adaptation_id=?",params![card_id,input.comic_adaptation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;
        frozen_cards.push(json!({"id":card_id,"body":json_value(frozen.0),"contentHash":frozen.1,"assetId":frozen.2}));
    }
    let context = json!({
        "scenePlanRevisionId": input.scene_plan_revision_id, "planningSceneStableKey": input.planning_scene_stable_key,
        "workingContext": working, "canon":{"id":input.canon_version_id,"body":json_value(canon_body)},
        "novelState":{"id":input.novel_state_version_id,"body":json_value(state_body)},
        "continuity":{"id":continuity,"body":json_value(continuity_body)},
        "adaptationPlanRevisionId": input.adaptation_plan_revision_id,
        "selectedEntities": frozen_entities, "visualCards": frozen_cards,
    });
    let fingerprint = request_hash(&context)?;
    let id = new_id("snapshot");
    let ts = now();
    tx.execute("INSERT INTO comic_scene_context_snapshots (id,novel_work_id,comic_adaptation_id,scene_plan_revision_id,planning_scene_stable_key,working_context_revision_id,novel_canon_version_id,novel_state_version_id,continuity_state_version_id,adaptation_plan_revision_id,resolver_contract_version,prompt_compiler_contract_version,context_fingerprint,resolved_context_json,resolved_context_hash,status,created_at) VALUES (?,?,?,?,?,?,?,?,?,?, 'scene-context.v1','prompt-compiler.v1',?,?,?,'provisional',?)",params![id,work.id,input.comic_adaptation_id,input.scene_plan_revision_id,input.planning_scene_stable_key,input.working_context_revision_id,input.canon_version_id,input.novel_state_version_id,continuity,input.adaptation_plan_revision_id,fingerprint,serde_json::to_string(&context).map_err(|e|e.to_string())?,fingerprint,ts]).map_err(|e|e.to_string())?;
    for (order, entity) in input.selected_entity_ids.iter().enumerate() {
        let frozen = context["selectedEntities"][order].clone();
        tx.execute("INSERT INTO comic_scene_context_entities (scene_context_snapshot_id,novel_entity_id,origin_context_revision_id,resolved_entity_hash,usage_role,source_order) VALUES (?,?,?,?,?,?)",params![id,entity,input.working_context_revision_id,request_hash(&frozen)?,"selected",order as i64]).map_err(|e|e.to_string())?;
    }
    for (order, card) in input.visual_card_revision_ids.iter().enumerate() {
        tx.execute("INSERT INTO comic_scene_context_visual_cards (scene_context_snapshot_id,comic_card_revision_id,usage_role,source_order) VALUES (?,?,?,?)",params![id,card,"selected",order as i64]).map_err(|e|e.to_string())?;
    }
    let result = snapshot_value(&tx, &id)?;
    tx.execute("INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?, 'novel_scene_context_resolve', ?, ?, ?, ?)",params![new_id("nreceipt"),input.idempotency_key,request,serde_json::to_string(&result).map_err(|e|e.to_string())?,ts]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
pub fn novel_scene_context_approve(
    state: tauri::State<'_, DbState>,
    input: SceneContextApproveInput,
) -> Result<SceneContextSnapshot, String> {
    db::with_connection(&state, |conn| scene_context_approve_inner(conn, input))
}

fn scene_context_approve_inner(
    conn: &Connection,
    input: SceneContextApproveInput,
) -> Result<SceneContextSnapshot, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let request = request_hash(
        &json!({"projectId":input.project_id,"novelWorkId":input.novel_work_id,"comicAdaptationId":input.comic_adaptation_id,"sceneContextSnapshotId":input.scene_context_snapshot_id,"contextFingerprint":input.context_fingerprint}),
    )?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if let Some((stored, response)) = tx.query_row("SELECT request_hash,response_json FROM novel_operation_receipts WHERE command_name='novel_scene_context_approve' AND idempotency_key=?",params![input.idempotency_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(|e|e.to_string())? {
        if stored != request { return Err("idempotencyKey 已用于不同业务载荷".into()); }
        return serde_json::from_str(&response).map_err(|_|"RECEIPT_CORRUPT".into());
    }
    adaptation_belongs_to_work(&tx, &input.project_id, &work.id, &input.comic_adaptation_id)?;
    tx.execute("INSERT INTO comic_scene_context_snapshot_approvals (scene_context_snapshot_id,approved_context_fingerprint,approved_by,approved_at) VALUES (?,?,?,?)",params![input.scene_context_snapshot_id,input.context_fingerprint,"local-user",now()]).map_err(|_|"SNAPSHOT_NOT_APPROVABLE".to_string())?;
    let changed = tx.execute("UPDATE comic_scene_context_snapshots SET status='approved' WHERE id=? AND novel_work_id=? AND comic_adaptation_id=? AND status='provisional' AND context_fingerprint=?",params![input.scene_context_snapshot_id,work.id,input.comic_adaptation_id,input.context_fingerprint]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("SNAPSHOT_NOT_APPROVABLE".into());
    }
    let result = snapshot_value(&tx, &input.scene_context_snapshot_id)?;
    let ts = now();
    tx.execute("INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?, 'novel_scene_context_approve', ?, ?, ?, ?)",params![new_id("nreceipt"),input.idempotency_key,request,serde_json::to_string(&result).map_err(|e|e.to_string())?,ts]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
pub fn novel_comic_plan_apply_preview(
    state: tauri::State<'_, DbState>,
    input: ComicPlanApplyPreviewInput,
) -> Result<ComicPlanPreview, String> {
    db::with_connection(&state, |conn| comic_plan_apply_preview_inner(conn, input))
}

fn comic_plan_apply_preview_inner(
    conn: &Connection,
    input: ComicPlanApplyPreviewInput,
) -> Result<ComicPlanPreview, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    adaptation_belongs_to_work(&tx, &input.project_id, &work.id, &input.comic_adaptation_id)?;
    let (continuity, optimistic): (Option<String>, i64) = tx.query_row("SELECT current_continuity_version_id,optimistic_version FROM comic_adaptations WHERE id=?",params![input.comic_adaptation_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    if continuity.as_deref() != Some(input.base_continuity_version_id.as_str())
        || optimistic != input.expected_adaptation_version
    {
        return Err("BASELINE_STALE".into());
    }
    let current_canon: Option<String> = tx
        .query_row(
            "SELECT published_canon_version_id FROM novel_works WHERE id=?",
            params![work.id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if current_canon.as_deref() != Some(input.base_canon_version_id.as_str()) {
        return Err("BASELINE_STALE".into());
    }
    let (proposal, scene_revision, _chapter_revision) = active_plan_head(
        &tx,
        &input.comic_adaptation_id,
        &input.accepted_plan_version_id,
    )?;
    let page_plan = adopted(
        &tx,
        &input.comic_adaptation_id,
        &input.page_panel_plan_revision_id,
        "page_panel_plan",
    )?;
    let (comic_chapter_draft_id, pages, mut counts) = page_plan_shape(&page_plan)?;
    let (planning_id, planning_key) = planning_chapter_for_page_plan(
        &tx,
        &input.comic_adaptation_id,
        &input.accepted_plan_version_id,
        &input.page_panel_plan_revision_id,
        &comic_chapter_draft_id,
    )?;
    let scene_body = adopted(
        &tx,
        &input.comic_adaptation_id,
        &scene_revision,
        "scene_plan",
    )?;
    let expected = scene_body
        .get("scenes")
        .and_then(Value::as_array)
        .ok_or("SCHEMA_INVALID")?
        .iter()
        .map(|v| {
            v.get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .ok_or("SCHEMA_INVALID".to_string())
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let mut selected = input.scene_context_selections.clone();
    selected.sort_by(|a, b| {
        a.planning_scene_stable_key
            .cmp(&b.planning_scene_stable_key)
            .then(
                a.scene_context_snapshot_id
                    .cmp(&b.scene_context_snapshot_id),
            )
    });
    let actual = selected
        .iter()
        .map(|v| v.planning_scene_stable_key.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if expected != actual || selected.len() != actual.len() {
        return Err("SCENE_SELECTION_INVALID".into());
    }
    for choice in &selected {
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM comic_scene_context_snapshots WHERE id=? AND novel_work_id=? AND comic_adaptation_id=? AND scene_plan_revision_id=? AND adaptation_plan_revision_id=? AND planning_scene_stable_key=? AND novel_canon_version_id=? AND continuity_state_version_id=? AND status='approved')",params![choice.scene_context_snapshot_id,work.id,input.comic_adaptation_id,scene_revision,proposal,choice.planning_scene_stable_key,input.base_canon_version_id,input.base_continuity_version_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid {
            return Err("SCENE_SELECTION_STALE".into());
        }
    }
    counts.scenes = selected.len();
    let tree = json!({"planningChapterId":planning_id,"planningChapterStableKey":planning_key,"pages":pages,"sceneSelections":selected});
    let fingerprint = request_hash(
        &json!({"type":"apply_comic_plan","adaptation":input.comic_adaptation_id,"head":input.accepted_plan_version_id,"pagePlan":input.page_panel_plan_revision_id,"canon":input.base_canon_version_id,"continuity":input.base_continuity_version_id,"expectedAdaptationVersion":input.expected_adaptation_version,"tree":tree}),
    )?;
    let existing: Option<(String,String,String)> = tx.query_row("SELECT id,preview_fingerprint,status FROM analysis_apply_operations WHERE idempotency_key=?",params![input.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
    let id = if let Some((id, old, status)) = existing {
        if old != fingerprint || status != "previewed" {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        };
        id
    } else {
        let id = new_id("apply");
        let ts = now();
        tx.execute("INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,expected_adaptation_version,idempotency_key,preview_fingerprint,approval_token_hash,approval_expires_at,status,created_at,updated_at) VALUES (?,'apply_comic_plan',NULL,?,?,?,?,?,?,?, 'previewed',?,?)",params![id,input.comic_adaptation_id,input.accepted_plan_version_id,input.expected_adaptation_version,input.idempotency_key,fingerprint,"pending",0_i64,ts,ts]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,'scene_input',0)",params![id,input.page_panel_plan_revision_id]).map_err(|e|e.to_string())?;
        for (order, choice) in selected.iter().enumerate() {
            tx.execute("INSERT INTO analysis_apply_scene_context_selections (apply_operation_id,planning_scene_stable_key,scene_context_snapshot_id,source_order) VALUES (?,?,?,?)",params![id,choice.planning_scene_stable_key,choice.scene_context_snapshot_id,order as i64]).map_err(|e|e.to_string())?;
        }
        id
    };
    let token = new_id("approval");
    let expires = now() + TOKEN_TTL_MS;
    tx.execute("UPDATE analysis_apply_operations SET approval_token_hash=?,approval_expires_at=?,updated_at=? WHERE id=? AND status='previewed'",params![request_hash(&json!(token))?,expires,now(),id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ComicPlanPreview {
        operation_id: id,
        comic_adaptation_id: input.comic_adaptation_id,
        approval_token: token,
        preview_fingerprint: fingerprint,
        expected_adaptation_version: input.expected_adaptation_version,
        tree,
        layout: page_plan.get("pages").cloned().unwrap_or_else(|| json!([])),
        counts,
        scene_context_selections: selected,
        expires_at: expires,
    })
}

#[tauri::command]
pub fn novel_comic_plan_apply(
    state: tauri::State<'_, DbState>,
    input: ComicPlanApplyInput,
) -> Result<ApplyResult, String> {
    db::with_connection(&state, |conn| comic_plan_apply_inner(conn, input))
}

fn comic_plan_apply_inner(
    conn: &Connection,
    input: ComicPlanApplyInput,
) -> Result<ApplyResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    adaptation_belongs_to_work(&tx, &input.project_id, &work.id, &input.comic_adaptation_id)?;
    let op: Option<(String, String, i64, String, String, String, i64)> = tx.query_row(
        "SELECT status,approval_token_hash,approval_expires_at,base_target_version_id,idempotency_key,comic_adaptation_id,expected_adaptation_version FROM analysis_apply_operations WHERE id=? AND operation_type='apply_comic_plan'",
        params![input.operation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).optional().map_err(|e|e.to_string())?;
    let (
        status,
        token_hash,
        expires,
        plan_head,
        stored_idempotency,
        op_adaptation,
        expected_version,
    ) = op.ok_or("OPERATION_UNKNOWN")?;
    if op_adaptation != input.comic_adaptation_id {
        return Err("OWNER_MISMATCH".into());
    }
    if stored_idempotency != input.idempotency_key {
        return Err("IDEMPOTENCY_MISMATCH".into());
    }
    if status == "succeeded" {
        let map = receipt_entity_map(&tx, &input.operation_id)?;
        return Ok(ApplyResult {
            operation_id: input.operation_id.clone(),
            status,
            receipt_id: Some(input.operation_id),
            entity_map: map,
            safe_error: None,
        });
    }
    if status != "previewed"
        || expires < now()
        || token_hash != request_hash(&json!(input.approval_token))?
    {
        return Err("APPROVAL_EXPIRED".into());
    }
    let (_proposal, scene_revision, _chapter_revision) =
        active_plan_head(&tx, &input.comic_adaptation_id, &plan_head)?;
    let page_revision: String = tx.query_row("SELECT analysis_artifact_revision_id FROM analysis_apply_operation_sources WHERE analysis_apply_operation_id=? ORDER BY source_order LIMIT 1",params![input.operation_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?.ok_or("REVISION_CONFLICT")?;
    let page_plan = adopted(
        &tx,
        &input.comic_adaptation_id,
        &page_revision,
        "page_panel_plan",
    )?;
    let (comic_chapter_draft_id, pages, _counts) = page_plan_shape(&page_plan)?;
    let (planning_id, planning_key) = planning_chapter_for_page_plan(
        &tx,
        &input.comic_adaptation_id,
        &plan_head,
        &page_revision,
        &comic_chapter_draft_id,
    )?;
    let choices = {
        let mut statement=tx.prepare("SELECT planning_scene_stable_key,scene_context_snapshot_id FROM analysis_apply_scene_context_selections WHERE apply_operation_id=? ORDER BY source_order,planning_scene_stable_key").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map(params![input.operation_id], |r| {
                Ok(ComicPlanSelection {
                    planning_scene_stable_key: r.get(0)?,
                    scene_context_snapshot_id: r.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    if choices.is_empty() {
        return Err("SCENE_SELECTION_INVALID".into());
    }
    let (canon, continuity): (Option<String>, Option<String>) = tx.query_row("SELECT work.published_canon_version_id,adaptation.current_continuity_version_id FROM novel_works work JOIN comic_adaptations adaptation ON adaptation.novel_work_id=work.id WHERE work.id=? AND adaptation.id=?",params![work.id,input.comic_adaptation_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    for choice in &choices {
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM comic_scene_context_snapshots WHERE id=? AND comic_adaptation_id=? AND scene_plan_revision_id=? AND novel_canon_version_id=? AND continuity_state_version_id=? AND status='approved')",params![choice.scene_context_snapshot_id,input.comic_adaptation_id,scene_revision,canon,continuity],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid {
            return Err("SCENE_SELECTION_STALE".into());
        }
    }
    let ts = now();
    let changed = tx.execute("UPDATE comic_adaptations SET optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND optimistic_version=?",params![ts,input.comic_adaptation_id,expected_version]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("REVISION_CONFLICT".into());
    }
    for choice in &choices {
        let changed=tx.execute("UPDATE comic_scene_context_snapshots SET status='frozen' WHERE id=? AND status='approved'",params![choice.scene_context_snapshot_id]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("SCENE_SELECTION_STALE".into());
        }
    }
    let production_chapter = new_id("production_chapter");
    tx.execute("INSERT INTO comic_production_chapters (id,comic_adaptation_id,comic_planning_chapter_id,page_panel_plan_revision_id,apply_operation_id,status,created_at) VALUES (?,?,?,?,?,'active',?)",params![production_chapter,input.comic_adaptation_id,planning_id,page_revision,input.operation_id,ts]).map_err(|e|e.to_string())?;
    let mut entity_map = Vec::new();
    entity_map.push(ApplyEntityMap {
        entity_kind: "production_chapter".into(),
        stable_key: planning_key,
        entity_id: production_chapter.clone(),
    });
    let mut scenes = std::collections::BTreeMap::new();
    for (index, choice) in choices.iter().enumerate() {
        let id = new_id("production_scene");
        tx.execute("INSERT INTO comic_production_scenes (id,comic_production_chapter_id,planning_scene_stable_key,scene_context_snapshot_id,scene_no,created_at) VALUES (?,?,?,?,?,?)",params![id,production_chapter,choice.planning_scene_stable_key,choice.scene_context_snapshot_id,(index+1) as i64,ts]).map_err(|e|e.to_string())?;
        scenes.insert(choice.planning_scene_stable_key.clone(), id.clone());
        entity_map.push(ApplyEntityMap {
            entity_kind: "production_scene".into(),
            stable_key: choice.planning_scene_stable_key.clone(),
            entity_id: id,
        });
    }
    for page in &pages {
        let page_key = page
            .get("stableKey")
            .and_then(Value::as_str)
            .ok_or("SCHEMA_INVALID")?;
        let page_no = page
            .get("pageNo")
            .and_then(Value::as_i64)
            .ok_or("SCHEMA_INVALID")?;
        let page_id = new_id("production_page");
        tx.execute("INSERT INTO comic_production_pages (id,comic_production_chapter_id,planning_page_stable_key,page_no,created_at) VALUES (?,?,?,?,?)",params![page_id,production_chapter,page_key,page_no,ts]).map_err(|e|e.to_string())?;
        entity_map.push(ApplyEntityMap {
            entity_kind: "production_page".into(),
            stable_key: page_key.into(),
            entity_id: page_id.clone(),
        });
        for panel in page
            .get("panels")
            .and_then(Value::as_array)
            .ok_or("SCHEMA_INVALID")?
        {
            let panel_key = panel
                .get("stableKey")
                .and_then(Value::as_str)
                .ok_or("SCHEMA_INVALID")?;
            let panel_no = panel
                .get("panelNo")
                .and_then(Value::as_i64)
                .ok_or("SCHEMA_INVALID")?;
            let scene_id = panel
                .get("planningSceneStableKey")
                .or_else(|| panel.get("sceneStableKey"))
                .and_then(Value::as_str)
                .map(|key| scenes.get(key).cloned().ok_or("SCENE_SELECTION_INVALID"))
                .transpose()?;
            let id = new_id("production_panel");
            tx.execute("INSERT INTO comic_production_panels (id,comic_production_page_id,comic_production_scene_id,planning_panel_stable_key,panel_no,spec_json,created_at) VALUES (?,?,?,?,?,?,?)",params![id,page_id,scene_id,panel_key,panel_no,serde_json::to_string(panel).map_err(|e|e.to_string())?,ts]).map_err(|e|e.to_string())?;
            entity_map.push(ApplyEntityMap {
                entity_kind: "production_panel".into(),
                stable_key: panel_key.into(),
                entity_id: id,
            });
        }
    }
    tx.execute("INSERT INTO analysis_apply_receipts (analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES (?,?,'apply_comic_plan',NULL,NULL,?)",params![input.operation_id,input.idempotency_key,ts]).map_err(|e|e.to_string())?;
    for entity in &entity_map {
        tx.execute("INSERT INTO analysis_apply_receipt_entity_maps (analysis_apply_operation_id,entity_kind,stable_key,entity_id,created_at) VALUES (?,?,?,?,?)",params![input.operation_id,entity.entity_kind,entity.stable_key,entity.entity_id,ts]).map_err(|e|e.to_string())?;
    }
    let changed=tx.execute("UPDATE comic_planning_chapters SET status='applied',updated_at=? WHERE id=? AND status='planning'",params![ts,planning_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("PLAN_HEAD_STALE".into());
    }
    let changed=tx.execute("UPDATE analysis_apply_operations SET status='succeeded',completed_at=?,updated_at=? WHERE id=? AND status='previewed'",params![ts,ts,input.operation_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("OPERATION_STATE_CONFLICT".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ApplyResult {
        operation_id: input.operation_id.clone(),
        status: "succeeded".into(),
        receipt_id: Some(input.operation_id),
        entity_map,
        safe_error: None,
    })
}

#[tauri::command]
pub fn novel_comic_apply_status(
    state: tauri::State<'_, DbState>,
    input: ComicApplyOperationInput,
) -> Result<ApplyResult, String> {
    db::with_connection(&state, |conn| comic_apply_operation_value(conn, input))
}

#[tauri::command]
pub fn novel_comic_apply_get_receipt(
    state: tauri::State<'_, DbState>,
    input: ComicApplyOperationInput,
) -> Result<ApplyResult, String> {
    db::with_connection(&state, |conn| comic_apply_operation_value(conn, input))
}

fn comic_apply_operation_value(
    conn: &Connection,
    input: ComicApplyOperationInput,
) -> Result<ApplyResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    adaptation_belongs_to_work(
        conn,
        &input.project_id,
        &work.id,
        &input.comic_adaptation_id,
    )?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Deferred)
        .map_err(|e| e.to_string())?;
    let status:String=tx.query_row("SELECT status FROM analysis_apply_operations WHERE id=? AND operation_type='apply_comic_plan' AND comic_adaptation_id=?",params![input.operation_id,input.comic_adaptation_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?.ok_or("OPERATION_UNKNOWN")?;
    let map = receipt_entity_map(&tx, &input.operation_id)?;
    Ok(ApplyResult {
        operation_id: input.operation_id,
        status,
        receipt_id: (!map.is_empty()).then(|| "apply_receipt".into()),
        entity_map: map,
        safe_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::novel::{
        list_artifacts_inner, novel_work_create_inner, NovelArtifactListInput, NovelWorkCreateInput,
    };

    fn strict_five_page_plan(dialogues: Value) -> Value {
        json!({"comicChapterDraftId":"chapter","pages":[{
            "stableKey":"page-1","pageNo":1,
            "layout":{"templateId":"hero_middle_5","layoutKind":"template","panelCount":5,"readingOrder":[1,2,3,4,5],"dominantPanel":3,"geometry":{"coordinateSystem":"normalized-0-1","panelCount":5,"readingOrder":[1,2,3,4,5],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[
                {"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.59,"y":0.04},{"x":0.54,"y":0.25},{"x":0.04,"y":0.25}],"bounds":{"x":0.04,"y":0.04,"width":0.55,"height":0.21},"textZone":{"x":0.08,"y":0.08,"width":0.34,"height":0.1}},
                {"panelNo":2,"polygon":[{"x":0.61,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.25},{"x":0.56,"y":0.25}],"bounds":{"x":0.56,"y":0.04,"width":0.40,"height":0.21},"textZone":{"x":0.67,"y":0.08,"width":0.2,"height":0.1}},
                {"panelNo":3,"polygon":[{"x":0.04,"y":0.27},{"x":0.96,"y":0.27},{"x":0.96,"y":0.66},{"x":0.04,"y":0.66}],"bounds":{"x":0.04,"y":0.27,"width":0.92,"height":0.39},"textZone":{"x":0.16,"y":0.37,"width":0.62,"height":0.12}},
                {"panelNo":4,"polygon":[{"x":0.04,"y":0.68},{"x":0.43,"y":0.68},{"x":0.43,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.68,"width":0.39,"height":0.28},"textZone":{"x":0.08,"y":0.75,"width":0.23,"height":0.1}},
                {"panelNo":5,"polygon":[{"x":0.45,"y":0.68},{"x":0.96,"y":0.68},{"x":0.96,"y":0.96},{"x":0.45,"y":0.96}],"bounds":{"x":0.45,"y":0.68,"width":0.51,"height":0.28},"textZone":{"x":0.57,"y":0.75,"width":0.28,"height":0.1}}
            ]}},
            "panels":[
                {"stableKey":"panel-1","panelNo":1,"planningSceneStableKey":"scene","dialogues":dialogues},
                {"stableKey":"panel-2","panelNo":2,"planningSceneStableKey":"scene"},
                {"stableKey":"panel-3","panelNo":3,"planningSceneStableKey":"scene"},
                {"stableKey":"panel-4","panelNo":4,"planningSceneStableKey":"scene"},
                {"stableKey":"panel-5","panelNo":5,"planningSceneStableKey":"scene"}
            ]
        }]})
    }

    fn adaptation_output_with_page_plan(page_plan: Value) -> Value {
        let envelope = |kind: &str, owner_type: &str, owner_id: &str, content: Value| json!({"schemaVersion":"novel-analysis.v1","artifactType":kind,"owner":{"ownerType":owner_type,"ownerId":owner_id},"content":content,"warnings":[]});
        json!({"artifacts":[
            envelope("adaptation_proposal","comic_adaptation","adapt",json!({"decisions":[{"stableKey":"decision","action":"keep","rationale":"保留","sourceRanges":[{"novelChapterRevisionId":"revision","startUtf8Byte":0,"endUtf8Byte":1,"confidence":1.0}],"targetHint":"chapter"}]})),
            envelope("comic_chapter_plan","comic_chapter","chapter",json!({"chapters":[{"stableKey":"chapter-plan","sourceSelections":[{"novelChapterRevisionId":"revision","startUtf8Byte":0,"endUtf8Byte":1,"confidence":1.0}],"goal":"目标","turn":"转折","hook":"钩子","pageBudget":1}]})),
            envelope("scene_plan","comic_chapter","chapter",json!({"comicChapterDraftId":"chapter","scenes":[{"order":1,"stableKey":"scene","goal":"目标","beats":[],"locationKeys":[],"characterKeys":[],"stateDelta":[]}]})),
            envelope("page_panel_plan","comic_chapter","chapter",page_plan)
        ]})
    }

    fn seed_adaptation_analysis_source(
        conn: &Connection,
    ) -> Result<(crate::novel::NovelWork, Adaptation, String, String), String> {
        let work = novel_work_create_inner(
            conn,
            NovelWorkCreateInput {
                project_id: "project-analysis".into(),
                title: "小说".into(),
                description: None,
                idempotency_key: "work-analysis".into(),
            },
        )?;
        let adaptation = novel_adaptation_create_inner(
            conn,
            AdaptationCreateInput {
                project_id: "project-analysis".into(),
                novel_work_id: work.id.clone(),
                title: "分支".into(),
                idempotency_key: "adaptation-analysis".into(),
            },
        )?;
        conn.execute("INSERT INTO novel_chapters(id,novel_work_id,volume_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES ('analysis-chapter',?,NULL,1,1,'第一章','analysis-revision',1,1)",params![work.id]).map_err(|e|e.to_string())?;
        conn.execute("INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,asset_id,requested_parent_context_revision_id,source_kind,created_at) VALUES ('analysis-revision','analysis-chapter',1,'正文','hash',NULL,NULL,'paste',1)",[]).map_err(|e|e.to_string())?;
        conn.execute("INSERT INTO comic_adaptation_chapters(id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES ('analysis-adaptation-chapter',?,'analysis-revision',1,1)",params![adaptation.id]).map_err(|e|e.to_string())?;
        conn.execute("INSERT INTO source_analysis_runs(id,novel_chapter_revision_id,novel_analysis_lineage_id,base_working_context_revision_id,base_canon_version_id,base_novel_state_version_id,frozen_comic_adaptation_id,frozen_comic_chapter_id,frozen_input_fingerprint,provider_id,model_id,status,prompt_version,schema_version,idempotency_key,progress_json,created_at,updated_at) VALUES ('source-ready','analysis-revision',?,NULL,?,?,?,?,'fp','provider','model','ready_for_review','v1','novel-analysis.v1','source-ready-key','{}',1,1)",params![work.current_analysis_lineage_id,work.published_canon_version_id,work.current_novel_state_version_id,adaptation.id,"analysis-adaptation-chapter"]).map_err(|e|e.to_string())?;
        for kind in ORIGINAL_ARTIFACT_TYPES {
            let artifact = format!("source-{kind}");
            let revision = format!("source-revision-{kind}");
            let body = match kind {
                "chapter_summary" => json!({"summary":"s","keyEvents":["e"]}),
                "chapter_beats" => json!({"beats":[{"stableKey":"b"}]}),
                "world_facts" | "character_facts" | "faction_facts" | "location_facts"
                | "prop_facts" => json!({"items":[{"stableKey":kind}]}),
                "timeline_delta" => json!({"events":[{"stableKey":"t"}]}),
                "continuity_delta" => json!({"changes":[{"stableKey":"c"}]}),
                "open_threads" => json!({"threads":[{"stableKey":"o"}]}),
                _ => unreachable!(),
            };
            conn.execute("INSERT INTO analysis_artifacts(id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES (?, 'source-ready',NULL,?,?,?,NULL,NULL,NULL,NULL,'active',0,1,1)",params![artifact,kind,work.id,"analysis-revision"]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,1,NULL,?,'','fixture','{}','{}','candidate',1)",params![revision,artifact,body.to_string()]).map_err(|e|e.to_string())?;
            conn.execute(
                "UPDATE analysis_artifacts SET candidate_head_revision_id=? WHERE id=?",
                params![revision, artifact],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok((
            work,
            adaptation,
            "analysis-adaptation-chapter".into(),
            "source-ready".into(),
        ))
    }

    #[test]
    fn adaptation_analysis_freezes_ten_sources_creates_explicit_chapter_and_recovers_only_expired()
    {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-analysis-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state,|conn|{
            let (work,adaptation,_chapter,source)=seed_adaptation_analysis_source(conn)?;
            // Requesting without the internal mapping makes the exact external
            // chapter revision create/select it; no implicit latest lookup.
            let input=AdaptationAnalysisStartInput {project_id:"project-analysis".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),comic_adaptation_chapter_id:None,source_analysis_run_id:Some(source),source_artifact_revision_ids:vec![],novel_chapter_revision_id:Some("analysis-revision".into()),base_canon_version_id:work.published_canon_version_id.clone(),base_novel_state_version_id:Some(work.current_novel_state_version_id.clone()),base_continuity_version_id:adaptation.current_continuity_version_id.clone().ok_or("C0")?,provider_id:None,model_id:None,comic_plan_intent:None,idempotency_key:"start".into()};
            assert!(serde_json::to_value(&input).map_err(|e| e.to_string())?.get("comicPlanIntent").is_none(), "legacy receipt request shape must omit absent intent");
            let started=start_adaptation_analysis_inner(conn,input.clone(),true,"configured_llm".into(),"model".into(),"session-a")?;
            assert!(started.dispatch);assert_eq!(started.run.status,"running");
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=?",params![started.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,10);
            assert_eq!(conn.query_row::<String,_,_>("SELECT novel_chapter_revision_id FROM comic_adaptation_chapters WHERE id=?",params![started.run.comic_adaptation_chapter_id],|r|r.get(0)).map_err(|e|e.to_string())?,"analysis-revision");
            let listed=adaptation_analysis_list_inner(conn,AdaptationAnalysisListInput{project_id:"project-analysis".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),comic_adaptation_chapter_id:Some(started.run.comic_adaptation_chapter_id.clone()),status:Some("running".into())})?;
            assert_eq!(listed.iter().map(|run|run.id.as_str()).collect::<Vec<_>>(),vec![started.run.id.as_str()]);
            assert!(adaptation_analysis_list_inner(conn,AdaptationAnalysisListInput{project_id:"foreign".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),comic_adaptation_chapter_id:None,status:None}).is_err());
            assert_eq!(start_adaptation_analysis_inner(conn,input,true,"configured_llm".into(),"model".into(),"session-a")?.run.id,started.run.id);
            conn.execute("UPDATE comic_adaptations SET config_json='{\"live\":true}' WHERE id=?",params![adaptation.id]).map_err(|e|e.to_string())?;
            assert_eq!(adaptation_analysis_prompt(conn,&started.run.id)?.adaptation_config,json!({}));
            assert_eq!(novel_adaptation_analysis_recover_stale_inner(conn,"project-analysis",&work.id,&adaptation.id)?,0);
            conn.execute("UPDATE adaptation_analysis_runs SET lease_expires_at=? WHERE id=?",params![now()-1,started.run.id]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE adaptation_analysis_run_attempts SET lease_expires_at=? WHERE adaptation_analysis_run_id=?",params![now()-1,started.run.id]).map_err(|e|e.to_string())?;
            assert_eq!(novel_adaptation_analysis_recover_stale_inner(conn,"project-analysis",&work.id,&adaptation.id)?,1);
            assert_eq!(conn.query_row::<String,_,_>("SELECT status FROM adaptation_analysis_runs WHERE id=?",params![started.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,"stale");
            let retried=retry_adaptation_analysis_inner(conn,AdaptationAnalysisRetryInput{project_id:"project-analysis".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),adaptation_analysis_run_id:started.run.id.clone(),idempotency_key:"retry".into()},false,"session-b")?;
            assert!(!retried.dispatch);assert_eq!(retried.run.status,"error");assert_eq!(retried.run.attempt_no,2);
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=?",params![started.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,10);
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM adaptation_analysis_run_attempts WHERE adaptation_analysis_run_id=? AND parent_attempt_id IS NOT NULL",params![started.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,1);
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_analysis_rejects_empty_shell_output_and_wrong_scene_owner() {
        let envelope = |kind: &str, owner_type: &str, owner_id: &str, content: Value| json!({"schemaVersion":"novel-analysis.v1","artifactType":kind,"owner":{"ownerType":owner_type,"ownerId":owner_id},"content":content,"warnings":[]});
        let empty = json!({"artifacts":[envelope("adaptation_proposal","comic_adaptation","adapt",json!({"decisions":[]})),envelope("comic_chapter_plan","comic_chapter","chapter",json!({"chapters":[]})),envelope("scene_plan","comic_chapter","chapter",json!({"comicChapterDraftId":"chapter","scenes":[]})),envelope("page_panel_plan","comic_chapter","chapter",json!({"comicChapterDraftId":"chapter","pages":[]}))]});
        assert!(adaptation_analysis_output(&empty, "adapt", "chapter", None).is_err());
        let wrong = json!({"artifacts":[envelope("adaptation_proposal","comic_adaptation","adapt",json!({"decisions":[{"stableKey":"d","action":"keep","rationale":"r","sourceRanges":[{"novelChapterRevisionId":"r","startUtf8Byte":0,"endUtf8Byte":1,"confidence":1.0}],"targetHint":"t"}]})),envelope("comic_chapter_plan","comic_chapter","chapter",json!({"chapters":[{"stableKey":"c","sourceSelections":[{"novelChapterRevisionId":"r","startUtf8Byte":0,"endUtf8Byte":1,"confidence":1.0}],"goal":"g","turn":"t","hook":"h","pageBudget":1}]})),envelope("scene_plan","comic_chapter","chapter",json!({"comicChapterDraftId":"wrong","scenes":[]})),envelope("page_panel_plan","comic_chapter","chapter",json!({"comicChapterDraftId":"chapter","pages":[]}))]});
        assert!(adaptation_analysis_output(&wrong, "adapt", "chapter", None).is_err());
    }

    #[test]
    fn adaptation_v5_freezes_content_identity_bindings_without_changing_v4_payload() {
        let make_prompt = |prompt_version: &str| AdaptationAnalysisPrompt {
            run_id: "run-1".into(),
            prompt_version: prompt_version.into(),
            novel_work_id: "work-1".into(),
            comic_adaptation_id: "adaptation-physical-id".into(),
            comic_adaptation_chapter_id: "chapter-physical-id".into(),
            model_id: "model".into(),
            base_canon: json!({}),
            base_state: json!({}),
            base_continuity: json!({}),
            working_context: json!({}),
            adaptation_config: json!({}),
            comic_plan_intent: None,
            comic_plan_intent_constraints: None,
            source_range_options: Some(vec![json!({
                "novelChapterRevisionId":"revision-1",
                "startUtf8Byte":0,
                "endUtf8Byte":4,
                "verifiedExcerpt":"正文",
            })]),
            inputs: vec![],
        };

        let v4 =
            adaptation_analysis_user_payload(&make_prompt(ADAPTATION_ANALYSIS_PROMPT_VERSION_V4));
        assert!(v4.get("outputIdentityBindings").is_none());
        assert_eq!(
            v4["sourceRangeOptions"][0]["novelChapterRevisionId"],
            "revision-1"
        );

        let v5 = adaptation_analysis_user_payload(&make_prompt(ADAPTATION_ANALYSIS_PROMPT_VERSION));
        assert_eq!(
            v5["outputIdentityBindings"],
            json!({
                "adaptation_proposal":{"owner":{"ownerType":"comic_adaptation","ownerId":"adaptation-physical-id"}},
                "comic_chapter_plan":{"owner":{"ownerType":"comic_chapter","ownerId":"chapter-physical-id"}},
                "scene_plan":{"owner":{"ownerType":"comic_chapter","ownerId":"chapter-physical-id"},"content":{"comicChapterDraftId":"chapter-physical-id"}},
                "page_panel_plan":{"owner":{"ownerType":"comic_chapter","ownerId":"chapter-physical-id"},"content":{"comicChapterDraftId":"chapter-physical-id"}},
            })
        );

        let chapter_plan = json!({"chapters":[{
            "stableKey":"planning-key-not-a-physical-id",
            "sourceSelections":[{"novelChapterRevisionId":"revision-1","startUtf8Byte":0,"endUtf8Byte":1,"confidence":1.0}],
            "goal":"目标","turn":"转折","hook":"钩子","pageBudget":1
        }]});
        assert!(validate_adaptation_content(
            "comic_chapter_plan",
            &chapter_plan,
            "chapter-physical-id",
        )
        .is_ok());
        let scene = json!({"comicChapterDraftId":"chapter-physical-id","scenes":[],"emptyReason":"本页没有独立场景"});
        assert!(validate_adaptation_content("scene_plan", &scene, "chapter-physical-id").is_ok());
        for invalid in [
            json!({"comicChapterDraftId":"planning-key-not-a-physical-id","scenes":[],"emptyReason":"无"}),
            json!({"scenes":[],"emptyReason":"无"}),
        ] {
            assert_eq!(
                validate_adaptation_content("scene_plan", &invalid, "chapter-physical-id")
                    .unwrap_err(),
                "SCENE_PLAN_CHAPTER_MISMATCH"
            );
        }
        for invalid in [
            json!({"comicChapterDraftId":"planning-key-not-a-physical-id","pages":[],"emptyReason":"无"}),
            json!({"pages":[],"emptyReason":"无"}),
        ] {
            assert_eq!(
                validate_adaptation_analysis_content(
                    "page_panel_plan",
                    &invalid,
                    "chapter-physical-id",
                )
                .unwrap_err(),
                "PAGE_PLAN_CHAPTER_MISMATCH"
            );
        }
    }

    #[test]
    fn adaptation_system_prompt_versions_keep_legacy_semantics_and_v5_adds_identity_bindings() {
        let v1 =
            adaptation_analysis_system_prompt_for_version(ADAPTATION_ANALYSIS_PROMPT_VERSION_V1)
                .unwrap();
        let v2 =
            adaptation_analysis_system_prompt_for_version(ADAPTATION_ANALYSIS_PROMPT_VERSION_V2)
                .unwrap();
        assert!(v2.contains(COMIC_PRODUCTION_CONTRACT));
        assert!(v2.contains("严格凸"));
        assert!(v2.contains("textZone"));
        assert!(v2.contains("comicPlanIntentConstraints"));
        assert!(!v1.contains(COMIC_PRODUCTION_CONTRACT));
        assert!(!v2.contains("几何模板片段"));

        let hero_constraints = json!({"pages":[{"layoutProfile":"hero_middle_5"}]});
        let hero_v3 = adaptation_analysis_system_prompt_for_frozen_constraints(
            ADAPTATION_ANALYSIS_PROMPT_VERSION_V3,
            Some(&hero_constraints),
        )
        .unwrap();
        assert!(hero_v3.contains("几何模板片段"));
        assert!(hero_v3.contains("\"templateId\":\"hero_middle_5\""));
        assert!(!hero_v3.contains("\"templateId\":\"reference_story_5\""));
        assert!(hero_v3.contains("允许且建议直接复用对应示例的完整 geometry 坐标"));
        assert!(!hero_v3.contains("sourceRangeOptions"));

        let hero_v4 = adaptation_analysis_system_prompt_for_frozen_constraints(
            ADAPTATION_ANALYSIS_PROMPT_VERSION_V4,
            Some(&hero_constraints),
        )
        .unwrap();
        assert!(hero_v4.contains("sourceRangeOptions"));
        assert!(hero_v4.contains("逐字复制 novelChapterRevisionId、startUtf8Byte、endUtf8Byte"));
        assert!(hero_v4.contains("verifiedExcerpt"));
        assert!(hero_v4.contains("\"templateId\":\"hero_middle_5\""));

        let hero_v5 = adaptation_analysis_system_prompt_for_frozen_constraints(
            ADAPTATION_ANALYSIS_PROMPT_VERSION,
            Some(&hero_constraints),
        )
        .unwrap();
        assert!(hero_v5.contains("outputIdentityBindings"));
        assert!(hero_v5.contains("comicChapterDraftId"));
        assert!(hero_v5.contains("绝不是 comic_chapter_plan.chapters[].stableKey"));
        assert!(hero_v5.contains("planningSceneStableKey"));

        let custom_constraints = json!({"pages":[{"layoutProfile":"custom_irregular"}]});
        let custom = adaptation_analysis_system_prompt_for_frozen_constraints(
            ADAPTATION_ANALYSIS_PROMPT_VERSION_V3,
            Some(&custom_constraints),
        )
        .unwrap();
        assert!(!custom.contains("几何模板片段"));
        assert!(!custom.contains("\"templateId\":\"hero_middle_5\""));
        assert!(custom.contains("必须自行生成合法的完整实际 geometry"));
        assert_ne!(
            adaptation_analysis_system_prompt_hash_for_frozen_constraints(
                ADAPTATION_ANALYSIS_PROMPT_VERSION,
                Some(&hero_constraints),
            )
            .unwrap(),
            adaptation_analysis_system_prompt_hash_for_frozen_constraints(
                ADAPTATION_ANALYSIS_PROMPT_VERSION,
                Some(&custom_constraints),
            )
            .unwrap(),
        );
        assert_eq!(
            adaptation_analysis_system_prompt_for_version("unknown").unwrap_err(),
            "FROZEN_PROMPT_VERSION_UNKNOWN"
        );
        assert_eq!(
            adaptation_analysis_system_prompt_hash_for_version(
                ADAPTATION_ANALYSIS_PROMPT_VERSION_V2
            )
            .unwrap(),
            format!("sha256:{:x}", Sha256::digest(v2.as_bytes()))
        );
    }

    #[test]
    fn shared_five_panel_layout_examples_pass_the_public_validator_and_profile_gate() {
        let examples: Value = serde_json::from_str(COMIC_PAGE_LAYOUT_EXAMPLES).unwrap();
        let profiles = examples["profiles"].as_object().unwrap();
        for (profile, layout) in profiles {
            let panels = layout["geometry"]["panels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|panel| json!({"panelNo": panel["panelNo"]}))
                .collect::<Vec<_>>();
            assert_eq!(layout["templateId"].as_str(), Some(profile.as_str()));
            assert!(validate_page_layout(layout, &panels).is_ok(), "{profile}");
            assert!(five_panel_profile_geometry_matches(layout), "{profile}");
        }
    }

    #[test]
    fn adaptation_output_keeps_public_layout_error_but_persists_safe_indexed_diagnostic() {
        let page = strict_five_page_plan(json!([]));
        let layout = &page["pages"][0]["layout"];
        let panels = page["pages"][0]["panels"].as_array().unwrap();
        assert!(validate_page_layout(layout, panels).is_ok());

        let mut bad_bounds = page.clone();
        bad_bounds["pages"][0]["layout"]["geometry"]["panels"][2]["bounds"]["width"] = json!(0.91);
        let bad_layout = &bad_bounds["pages"][0]["layout"];
        let bad_panels = bad_bounds["pages"][0]["panels"].as_array().unwrap();
        assert_eq!(
            validate_page_layout(bad_layout, bad_panels),
            Err("LAYOUT_INVALID".into())
        );
        assert_eq!(
            page_plan_shape_for_adaptation_analysis(&bad_bounds)
                .err()
                .unwrap(),
            "LAYOUT_INVALID:BOUNDS:pages[0].layout.geometry.panels[2].bounds"
        );

        let mut bad_gutter = page.clone();
        bad_gutter["pages"][0]["layout"]["geometry"]["gutter"] = json!(0.011);
        assert_eq!(
            page_plan_shape_for_adaptation_analysis(&bad_gutter)
                .err()
                .unwrap(),
            "LAYOUT_INVALID:GUTTER:pages[0].layout.geometry.gutter"
        );

        let mut bad_text_zone = page.clone();
        bad_text_zone["pages"][0]["layout"]["geometry"]["panels"][1]["textZone"]["width"] =
            json!(0.07);
        assert_eq!(
            page_plan_shape_for_adaptation_analysis(&bad_text_zone)
                .err()
                .unwrap(),
            "LAYOUT_INVALID:TEXT_ZONE:pages[0].layout.geometry.panels[1].textZone"
        );

        let diagnostic = adaptation_analysis_output(
            &adaptation_output_with_page_plan(bad_bounds),
            "adapt",
            "chapter",
            None,
        )
        .unwrap_err();
        assert_eq!(
            diagnostic,
            "LAYOUT_INVALID:BOUNDS:pages[0].layout.geometry.panels[2].bounds"
        );
        let (code, message) = adaptation_analysis_failure_fields(diagnostic);
        assert_eq!(code, "LAYOUT_INVALID");
        assert_eq!(
            message,
            "漫画页面布局未通过本地几何校验：LAYOUT_INVALID:BOUNDS:pages[0].layout.geometry.panels[2].bounds"
        );
        assert!(!message.contains("模型"));
        let (code, message) = adaptation_analysis_failure_fields(
            "LAYOUT_INVALID:provider detail must not become a local diagnostic".into(),
        );
        assert_eq!(code, "ANALYSIS_FAILED");
        assert_eq!(
            message,
            "改编分析返回了无法识别的布局错误，未开始生成漫画页。"
        );
        for (error, expected_code, expected_message) in [
            (
                "EVIDENCE_RANGE_INVALID",
                "EVIDENCE_RANGE_INVALID",
                "改编引用不是有效的 UTF-8 字节范围，未生成漫画页。",
            ),
            (
                "EVIDENCE_SOURCE_SCOPE_INVALID",
                "EVIDENCE_SOURCE_SCOPE_INVALID",
                "改编引用不属于当前小说，未生成漫画页。",
            ),
            (
                "EVIDENCE_SOURCE_READ_FAILED",
                "EVIDENCE_SOURCE_READ_FAILED",
                "无法读取改编引用的冻结正文，未生成漫画页。",
            ),
            (
                "EVIDENCE_RANGE_OPTION_MISMATCH",
                "EVIDENCE_RANGE_OPTION_MISMATCH",
                "改编引用未使用本次冻结的来源范围。",
            ),
        ] {
            let (code, message) = adaptation_analysis_failure_fields(error.into());
            assert_eq!(code, expected_code);
            assert_eq!(message, expected_message);
        }
        let (code, message) = adaptation_analysis_failure_fields(
            "EVIDENCE_RANGE_INVALID:untrusted provider payload".into(),
        );
        assert_eq!(code, "ANALYSIS_FAILED");
        assert_eq!(message, "EVIDENCE_RANGE_INVALID:untrusted provider payload");
    }

    #[test]
    fn adaptation_layout_diagnostic_is_written_to_the_run_and_attempt() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-layout-diagnostic-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, source) = seed_adaptation_analysis_source(conn)?;
            let continuity = adaptation.current_continuity_version_id.clone().ok_or("C0")?;
            let source_for_fingerprint = source.clone();
            let started = start_adaptation_analysis_inner(
                conn,
                AdaptationAnalysisStartInput {
                    project_id: "project-analysis".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(), comic_adaptation_chapter_id: Some(chapter.clone()),
                    source_analysis_run_id: Some(source), source_artifact_revision_ids: vec![], novel_chapter_revision_id: Some("analysis-revision".into()),
                    base_canon_version_id: work.published_canon_version_id.clone(), base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()), base_continuity_version_id: continuity.clone(), provider_id: None, model_id: None, comic_plan_intent: None, idempotency_key: "layout-diagnostic".into(),
                }, true, "configured_llm".into(), "model".into(), "owner",
            )?;
            let prompt_version: String = conn.query_row(
                "SELECT prompt_version FROM adaptation_analysis_runs WHERE id=?",
                params![started.run.id], |row| row.get(0),
            ).map_err(|e| e.to_string())?;
            assert_eq!(prompt_version, ADAPTATION_ANALYSIS_PROMPT_VERSION);
            let event: Value = conn.query_row(
                "SELECT payload_json FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id], |row| row.get::<_, String>(0),
            ).map(json_value).map_err(|e| e.to_string())?;
            let expected_system_prompt_hash = adaptation_analysis_system_prompt_hash_for_version(
                ADAPTATION_ANALYSIS_PROMPT_VERSION,
            )?;
            assert_eq!(event["promptVersion"].as_str(), Some(ADAPTATION_ANALYSIS_PROMPT_VERSION));
            assert_eq!(
                event["systemPromptHash"].as_str(),
                Some(expected_system_prompt_hash.as_str())
            );
            let expected_source_range_options = vec![json!({
                "novelChapterRevisionId":"analysis-revision",
                "startUtf8Byte":0,
                "endUtf8Byte":"正文".len() as i64,
                "verifiedExcerpt":"正文",
            })];
            assert_eq!(
                event["sourceRangeOptions"],
                Value::Array(expected_source_range_options.clone())
            );
            let expected_output_identity_bindings = json!({
                "adaptation_proposal":{"owner":{"ownerType":"comic_adaptation","ownerId":adaptation.id}},
                "comic_chapter_plan":{"owner":{"ownerType":"comic_chapter","ownerId":chapter}},
                "scene_plan":{"owner":{"ownerType":"comic_chapter","ownerId":chapter},"content":{"comicChapterDraftId":chapter}},
                "page_panel_plan":{"owner":{"ownerType":"comic_chapter","ownerId":chapter},"content":{"comicChapterDraftId":chapter}},
            });
            assert_eq!(event["outputIdentityBindings"], expected_output_identity_bindings);
            let prompt = adaptation_analysis_prompt(conn, &started.run.id)?;
            assert_eq!(
                prompt.source_range_options,
                Some(expected_source_range_options.clone())
            );
            assert_eq!(
                adaptation_analysis_user_payload(&prompt)["sourceRangeOptions"],
                Value::Array(expected_source_range_options.clone())
            );
            assert_eq!(
                adaptation_analysis_user_payload(&prompt)["outputIdentityBindings"],
                expected_output_identity_bindings
            );
            let frozen_inputs = ORIGINAL_ARTIFACT_TYPES.iter().map(|kind| {
                let revision = format!("source-revision-{kind}");
                let body: String = conn.query_row(
                    "SELECT body_json FROM analysis_artifact_revisions WHERE id=?",
                    params![revision], |row| row.get(0),
                ).map_err(|e| e.to_string())?;
                Ok((kind.to_string(), revision, format!("sha256:{:x}", Sha256::digest(body.as_bytes()))))
            }).collect::<Result<Vec<_>, String>>()?;
            let expected_fingerprint = request_hash(&json!({
                "mode":"source_run","sourceRun":source_for_fingerprint,"inputs":frozen_inputs,
                "canon":work.published_canon_version_id,"state":Some(work.current_novel_state_version_id),"continuity":continuity,
                "adaptation":adaptation.id,"chapter":chapter,"provider":"configured_llm","model":"model","branchConfig":json!({}),
                "comicPlanIntent":Option::<ComicPlanIntent>::None,"comicPlanIntentConstraints":Option::<Value>::None,
                "sourceRangeOptions":expected_source_range_options,
                "outputIdentityBindings":expected_output_identity_bindings,
                "promptVersion":ADAPTATION_ANALYSIS_PROMPT_VERSION,
                "systemPromptHash":expected_system_prompt_hash,
                "schemaHash":format!("sha256:{:x}",Sha256::digest(ADAPTATION_ANALYSIS_SCHEMA.as_bytes()))
            }))?;
            let stored_fingerprint: String = conn.query_row(
                "SELECT frozen_input_fingerprint FROM adaptation_analysis_runs WHERE id=?",
                params![started.run.id], |row| row.get(0),
            ).map_err(|e| e.to_string())?;
            assert_eq!(stored_fingerprint, expected_fingerprint);

            let diagnostic = "LAYOUT_INVALID:BOUNDS:pages[0].layout.geometry.panels[2].bounds";
            let (code, message) = adaptation_analysis_failure_fields(diagnostic.into());
            let failed = fail_adaptation_analysis_inner(conn, &started.run.id, "owner", code, &message)?;
            assert_eq!(failed.safe_error_code.as_deref(), Some("LAYOUT_INVALID"));
            assert_eq!(failed.safe_user_message.as_deref(), Some(message.as_str()));
            let attempt: (String, String) = conn.query_row(
                "SELECT safe_error_code,safe_user_message FROM adaptation_analysis_run_attempts WHERE adaptation_analysis_run_id=? AND attempt_no=1",
                params![started.run.id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e| e.to_string())?;
            assert_eq!(attempt.0, "LAYOUT_INVALID");
            assert_eq!(attempt.1, message);
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_prompt_rejects_tampered_frozen_intent_constraints() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-intent-freeze-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, _, source) = seed_adaptation_analysis_source(conn)?;
            let started = start_adaptation_analysis_inner(
                conn,
                AdaptationAnalysisStartInput {
                    project_id: "project-analysis".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(), comic_adaptation_chapter_id: None,
                    source_analysis_run_id: Some(source), source_artifact_revision_ids: vec![], novel_chapter_revision_id: Some("analysis-revision".into()),
                    base_canon_version_id: work.published_canon_version_id.clone(), base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()), base_continuity_version_id: adaptation.current_continuity_version_id.clone().ok_or("C0")?, provider_id: None, model_id: None,
                    comic_plan_intent: Some(ComicPlanIntent { pages: vec![crate::novel::ComicPlanPageIntent { panel_count: 5, layout_profile: Some("hero_middle_5".into()), dialogues: None }] }),
                    idempotency_key: "intent-freeze".into(),
                },
                true, "configured_llm".into(), "model".into(), "owner",
            )?;
            let prompt = adaptation_analysis_prompt(conn, &started.run.id)?;
            assert!(prompt.comic_plan_intent.is_some());
            let expected_hash = adaptation_analysis_system_prompt_hash_for_frozen_constraints(
                ADAPTATION_ANALYSIS_PROMPT_VERSION,
                prompt.comic_plan_intent_constraints.as_ref(),
            )?;
            let event: Value = conn.query_row(
                "SELECT payload_json FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id],
                |row| row.get::<_, String>(0),
            )
            .map(json_value)
            .map_err(|error| error.to_string())?;
            assert_eq!(event["promptVersion"].as_str(), Some(ADAPTATION_ANALYSIS_PROMPT_VERSION));
            assert_eq!(event["systemPromptHash"].as_str(), Some(expected_hash.as_str()));
            conn.execute(
                "UPDATE adaptation_analysis_run_events SET payload_json=json_set(payload_json,'$.comicPlanIntentConstraints.pages[0].panelCount',4) WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                adaptation_analysis_prompt(conn, &started.run.id).err().unwrap(),
                "FROZEN_COMIC_PLAN_INTENT_CONSTRAINTS_MISMATCH"
            );
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_prompt_rebuilds_v5_source_ranges_and_keeps_legacy_runs_absent() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-source-ranges-freeze-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, source) = seed_adaptation_analysis_source(conn)?;
            let started = start_adaptation_analysis_inner(
                conn,
                AdaptationAnalysisStartInput {
                    project_id: "project-analysis".into(),
                    novel_work_id: work.id.clone(),
                    comic_adaptation_id: adaptation.id.clone(),
                    comic_adaptation_chapter_id: Some(chapter),
                    source_analysis_run_id: Some(source),
                    source_artifact_revision_ids: vec![],
                    novel_chapter_revision_id: Some("analysis-revision".into()),
                    base_canon_version_id: work.published_canon_version_id.clone(),
                    base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()),
                    base_continuity_version_id: adaptation
                        .current_continuity_version_id
                        .clone()
                        .ok_or("C0")?,
                    provider_id: None,
                    model_id: None,
                    comic_plan_intent: None,
                    idempotency_key: "source-ranges-freeze".into(),
                },
                true,
                "configured_llm".into(),
                "model".into(),
                "owner",
            )?;
            assert!(adaptation_analysis_prompt(conn, &started.run.id)?
                .source_range_options
                .is_some());
            conn.execute(
                "UPDATE adaptation_analysis_run_events
                 SET payload_json=json_remove(payload_json,'$.outputIdentityBindings')
                 WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id],
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                adaptation_analysis_prompt(conn, &started.run.id).err().unwrap(),
                "FROZEN_OUTPUT_IDENTITY_BINDINGS_INVALID"
            );
            let expected_bindings = adaptation_output_identity_bindings(
                &started.run.comic_adaptation_id,
                &started.run.comic_adaptation_chapter_id,
            )
            .to_string();
            conn.execute(
                "UPDATE adaptation_analysis_run_events
                 SET payload_json=json_set(payload_json,'$.outputIdentityBindings',json(?))
                 WHERE adaptation_analysis_run_id=? AND seq=1",
                params![expected_bindings, started.run.id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE adaptation_analysis_run_events
                 SET payload_json=json_set(payload_json,'$.outputIdentityBindings.scene_plan.content.comicChapterDraftId','tampered')
                 WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id],
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                adaptation_analysis_prompt(conn, &started.run.id).err().unwrap(),
                "FROZEN_OUTPUT_IDENTITY_BINDINGS_MISMATCH"
            );
            conn.execute(
                "UPDATE adaptation_analysis_run_events
                 SET payload_json=json_set(payload_json,'$.outputIdentityBindings',json(?))
                 WHERE adaptation_analysis_run_id=? AND seq=1",
                params![expected_bindings, started.run.id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE adaptation_analysis_run_events SET payload_json=json_set(payload_json,'$.sourceRangeOptions[0].endUtf8Byte',1) WHERE adaptation_analysis_run_id=? AND seq=1",
                params![started.run.id],
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                adaptation_analysis_prompt(conn, &started.run.id).err().unwrap(),
                "FROZEN_SOURCE_RANGE_OPTIONS_MISMATCH"
            );

            let legacy_run_id = "legacy-source-ranges-freeze";
            conn.execute(
                "INSERT INTO adaptation_analysis_runs (id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at)
                 SELECT ?,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,?,schema_version,frozen_input_fingerprint,?,'draft',1,NULL,NULL,NULL,NULL,NULL,created_at,updated_at,NULL
                 FROM adaptation_analysis_runs WHERE id=?",
                params![legacy_run_id, ADAPTATION_ANALYSIS_PROMPT_VERSION_V3, "legacy-source-ranges-freeze-key", started.run.id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "INSERT INTO adaptation_analysis_run_inputs (adaptation_analysis_run_id,artifact_type,analysis_artifact_revision_id,source_order)
                 SELECT ?,artifact_type,analysis_artifact_revision_id,source_order FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=?",
                params![legacy_run_id, started.run.id],
            )
            .map_err(|error| error.to_string())?;
            let legacy_expiry = now() + ADAPTATION_ANALYSIS_LEASE_MS;
            conn.execute(
                "INSERT INTO adaptation_analysis_run_attempts (id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,created_at)
                 VALUES (?,?,1,NULL,'queued','owner',?,?,?)",
                params![new_id("adaptattempt"), legacy_run_id, legacy_expiry, now(), now()],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE adaptation_analysis_runs SET status='queued',lease_owner='owner',lease_expires_at=?,heartbeat_at=?,updated_at=? WHERE id=? AND status='draft'",
                params![legacy_expiry, now(), now(), legacy_run_id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE adaptation_analysis_run_attempts SET status='running' WHERE adaptation_analysis_run_id=? AND attempt_no=1 AND status='queued'",
                params![legacy_run_id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE adaptation_analysis_runs SET status='running',updated_at=? WHERE id=? AND status='queued'",
                params![now(), legacy_run_id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "INSERT INTO adaptation_analysis_run_events (id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at)
                 SELECT ?,?,1,event_type,json_set(json_remove(json_remove(payload_json,'$.sourceRangeOptions'),'$.outputIdentityBindings'),'$.promptVersion',?),created_at
                 FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=? AND seq=1",
                params![new_id("adaptevent"), legacy_run_id, ADAPTATION_ANALYSIS_PROMPT_VERSION_V3, started.run.id],
            )
            .map_err(|error| error.to_string())?;
            let legacy_prompt = adaptation_analysis_prompt(conn, legacy_run_id)?;
            assert!(legacy_prompt.source_range_options.is_none());
            assert!(adaptation_analysis_user_payload(&legacy_prompt)
                .get("sourceRangeOptions")
                .is_none());
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn comic_plan_intent_validates_profile_position_and_dialogue_tristate() {
        let exact = ComicPlanIntent {
            pages: vec![crate::novel::ComicPlanPageIntent {
                panel_count: 5,
                layout_profile: Some("hero_middle_5".into()),
                dialogues: Some(vec![ComicPlanDialogueIntent {
                    panel_no: 1,
                    speaker: "小川".into(),
                    text: "信不能湿。".into(),
                }]),
            }],
        };
        let page = strict_five_page_plan(json!([{"speaker":"小川","text":"信不能湿。"}]));
        assert!(validate_comic_plan_intent_page_plan(&exact, &page).is_ok());
        let mut swapped = page.clone();
        let geometry = swapped["pages"][0]["layout"]["geometry"]["panels"]
            .as_array_mut()
            .unwrap();
        let first = geometry[0].clone();
        let second = geometry[1].clone();
        geometry[0] = second;
        geometry[0]["panelNo"] = json!(1);
        geometry[1] = first;
        geometry[1]["panelNo"] = json!(2);
        assert_eq!(
            validate_comic_plan_intent_page_plan(&exact, &swapped).unwrap_err(),
            "COMIC_PLAN_INTENT_LAYOUT_MISMATCH"
        );
        let silent = ComicPlanIntent {
            pages: vec![crate::novel::ComicPlanPageIntent {
                panel_count: 5,
                layout_profile: Some("hero_middle_5".into()),
                dialogues: Some(vec![]),
            }],
        };
        assert_eq!(
            validate_comic_plan_intent_page_plan(&silent, &page).unwrap_err(),
            "COMIC_PLAN_INTENT_DIALOGUE_MISMATCH"
        );
        let unconstrained = ComicPlanIntent {
            pages: vec![crate::novel::ComicPlanPageIntent {
                panel_count: 5,
                layout_profile: Some("hero_middle_5".into()),
                dialogues: None,
            }],
        };
        assert!(validate_comic_plan_intent_page_plan(&unconstrained, &page).is_ok());
    }

    #[test]
    fn adaptation_analysis_artifact_revision_mode_freezes_only_adopted_heads() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-analysis-adopted-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, _) = seed_adaptation_analysis_source(conn)?;
            let mut revisions = Vec::new();
            for kind in ORIGINAL_ARTIFACT_TYPES {
                let artifact = format!("source-{kind}");
                let revision = format!("source-revision-{kind}");
                conn.execute("UPDATE analysis_artifact_revisions SET status='adopted' WHERE id=?",params![revision]).map_err(|e|e.to_string())?;
                conn.execute("UPDATE analysis_artifacts SET adopted_head_revision_id=? WHERE id=?",params![revision,artifact]).map_err(|e|e.to_string())?;
                revisions.push(revision);
            }
            let result=start_adaptation_analysis_inner(conn,AdaptationAnalysisStartInput { project_id:"project-analysis".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),comic_adaptation_chapter_id:Some(chapter),source_analysis_run_id:None,source_artifact_revision_ids:revisions,novel_chapter_revision_id:Some("analysis-revision".into()),base_canon_version_id:work.published_canon_version_id.clone(),base_novel_state_version_id:Some(work.current_novel_state_version_id.clone()),base_continuity_version_id:adaptation.current_continuity_version_id.clone().ok_or("C0")?,provider_id:None,model_id:None,comic_plan_intent:None,idempotency_key:"adopted-start".into()},false,"configured_llm".into(),"model".into(),"owner")?;
            assert_eq!(result.run.status,"error");
            assert_eq!(conn.query_row::<String,_,_>("SELECT input_mode FROM adaptation_analysis_runs WHERE id=?",params![result.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,"artifact_revisions");
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=?",params![result.run.id],|r|r.get(0)).map_err(|e|e.to_string())?,10);
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn artifact_list_filters_adaptation_analysis_run_without_hiding_other_runs() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-analysis-list-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, source) = seed_adaptation_analysis_source(conn)?;
            let make_run = |key: &str| {
                start_adaptation_analysis_inner(
                    conn,
                    AdaptationAnalysisStartInput {
                        project_id: "project-analysis".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(), comic_adaptation_chapter_id: Some(chapter.clone()),
                        source_analysis_run_id: Some(source.clone()), source_artifact_revision_ids: vec![], novel_chapter_revision_id: Some("analysis-revision".into()),
                        base_canon_version_id: work.published_canon_version_id.clone(), base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()), base_continuity_version_id: adaptation.current_continuity_version_id.clone().ok_or("C0")?, provider_id: None, model_id: None, comic_plan_intent: None, idempotency_key: key.into(),
                    }, true, "configured_llm".into(), "model".into(), "owner",
                )
            };
            let first = make_run("list-run-1")?.run;
            let second = make_run("list-run-2")?.run;
            for run in [&first, &second] {
                for kind in ADAPTATION_ARTIFACT_TYPES {
                    let chapter_scope = (kind != "adaptation_proposal").then(|| chapter.clone());
                    conn.execute("INSERT INTO analysis_artifacts(id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES (?,NULL,?,?,?,?,?,?,NULL,NULL,'active',0,?,?)",params![format!("{}-{kind}",run.id),run.id,kind,work.id,"analysis-revision",adaptation.id,chapter_scope,now(),now()]).map_err(|e|e.to_string())?;
                }
            }
            let base = NovelArtifactListInput {project_id:"project-analysis".into(),novel_work_id:work.id.clone(),artifact_types:None,comic_adaptation_id:Some(adaptation.id.clone()),comic_chapter_id:None,adaptation_analysis_run_id:None,revision_status:None};
            assert_eq!(list_artifacts_inner(conn,&base)?.len(),8);
            let selected = NovelArtifactListInput {adaptation_analysis_run_id:Some(first.id.clone()),..base};
            let items=list_artifacts_inner(conn,&selected)?;
            assert_eq!(items.len(),4);
            assert!(items.iter().all(|item|item.adaptation_analysis_run_id.as_deref()==Some(first.id.as_str())));
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_analysis_completion_writes_exactly_four_scoped_outputs() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-analysis-complete-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, source) = seed_adaptation_analysis_source(conn)?;
            let started = start_adaptation_analysis_inner(
                conn,
                AdaptationAnalysisStartInput {
                    project_id: "project-analysis".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(), comic_adaptation_chapter_id: Some(chapter.clone()),
                    source_analysis_run_id: Some(source), source_artifact_revision_ids: vec![], novel_chapter_revision_id: Some("analysis-revision".into()),
                    base_canon_version_id: work.published_canon_version_id.clone(), base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()), base_continuity_version_id: adaptation.current_continuity_version_id.clone().ok_or("C0")?, provider_id: None, model_id: None, comic_plan_intent: None, idempotency_key: "complete-start".into(),
                }, true, "configured_llm".into(), "model".into(), "owner",
            )?;
            let reference = json!({"novelChapterRevisionId":"analysis-revision","startUtf8Byte":0,"endUtf8Byte":"正文".len(),"confidence":1.0});
            let geometry = json!({"coordinateSystem":"normalized-0-1","panelCount":1,"readingOrder":[1],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[{"panelNo":1,"polygon":[{"x":0.0,"y":0.0},{"x":1.0,"y":0.0},{"x":1.0,"y":1.0},{"x":0.0,"y":1.0}],"bounds":{"x":0.0,"y":0.0,"width":1.0,"height":1.0},"textZone":{"x":0.2,"y":0.2,"width":0.2,"height":0.1},"bleed":{"top":true,"right":true,"bottom":true,"left":true}}]});
            let outputs = vec![
                ("adaptation_proposal".into(),json!({"decisions":[{"stableKey":"decision","action":"keep","rationale":"保留","sourceRanges":[reference.clone()],"targetHint":"chapter"}]})),
                ("comic_chapter_plan".into(),json!({"chapters":[{"stableKey":"draft","sourceSelections":[reference],"goal":"目标","turn":"转折","hook":"钩子","pageBudget":1}]})),
                ("scene_plan".into(),json!({"comicChapterDraftId":chapter,"scenes":[{"stableKey":"scene","order":1,"goal":"目标","beats":[],"locationKeys":[],"characterKeys":[],"stateDelta":[]}]})),
                ("page_panel_plan".into(),json!({"comicChapterDraftId":chapter,"pages":[{"stableKey":"page","pageNo":1,"layout":{"templateId":"custom_irregular","layoutKind":"custom_irregular","panelCount":1,"readingOrder":[1],"dominantPanel":1,"geometry":geometry},"panels":[{"stableKey":"panel","panelNo":1,"planningSceneStableKey":"scene"}]}]})),
            ];
            let mut legal_but_not_frozen = outputs.clone();
            legal_but_not_frozen
                .iter_mut()
                .find(|(kind, _)| kind == "comic_chapter_plan")
                .ok_or("TEST_CHAPTER_PLAN_MISSING")?
                .1["chapters"][0]["sourceSelections"][0]["endUtf8Byte"] = json!("正".len() as i64);
            assert_eq!(
                complete_adaptation_analysis_inner(
                    conn,
                    &started.run.id,
                    legal_but_not_frozen,
                    "owner",
                )
                .unwrap_err(),
                "EVIDENCE_RANGE_OPTION_MISMATCH"
            );
            assert_eq!(
                conn.query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM analysis_artifacts WHERE adaptation_analysis_run_id=?",
                    params![started.run.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
                0
            );
            let ready=complete_adaptation_analysis_inner(conn,&started.run.id,outputs,"owner")?;
            assert_eq!(ready.status,"ready_for_review");
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM adaptation_analysis_run_artifacts WHERE adaptation_analysis_run_id=?",params![ready.id],|r|r.get(0)).map_err(|e|e.to_string())?,4);
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM analysis_artifacts WHERE adaptation_analysis_run_id=? AND artifact_type IN ('adaptation_proposal','comic_chapter_plan','scene_plan','page_panel_plan')",params![ready.id],|r|r.get(0)).map_err(|e|e.to_string())?,4);
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    fn insert_adopted_artifact(
        conn: &Connection,
        work_id: &str,
        adaptation_id: &str,
        artifact_id: &str,
        revision_id: &str,
        artifact_type: &str,
        body: Value,
    ) -> Result<(), String> {
        let (chapter_id,project_id,canon,state,continuity):(String,String,String,String,String)=conn.query_row("SELECT chapter.id,work.project_id,work.published_canon_version_id,work.current_novel_state_version_id,adaptation.current_continuity_version_id FROM comic_adaptation_chapters chapter JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id JOIN novel_works work ON work.id=adaptation.novel_work_id WHERE adaptation.id=? AND work.id=? LIMIT 1",params![adaptation_id,work_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|e|e.to_string())?;
        conn.execute("INSERT OR IGNORE INTO adaptation_analysis_runs(id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,heartbeat_at,created_at,updated_at) VALUES ('fixture-analysis-run',?,?,?,?,'artifact_revisions',NULL,?,?,?,'fixture','fixture','fixture','novel-analysis.v1','fixture','fixture-analysis-run','running',1,'fixture',999999999999,1,1,1)",params![project_id,work_id,adaptation_id,chapter_id,canon,state,continuity]).map_err(|e|e.to_string())?;
        let chapter_scope = matches!(
            artifact_type,
            "comic_chapter_plan" | "scene_plan" | "page_panel_plan"
        )
        .then_some(chapter_id);
        conn.execute("INSERT INTO analysis_artifacts (id,adaptation_analysis_run_id,artifact_type,novel_work_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES (?,'fixture-analysis-run',?,?,?,?,'active',0,1,1)",params![artifact_id,artifact_type,work_id,adaptation_id,chapter_scope]).map_err(|e|e.to_string())?;
        conn.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,1,NULL,?,'','fixture','{}','{}','adopted',1)",params![revision_id,artifact_id,serde_json::to_string(&body).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        conn.execute(
            "UPDATE analysis_artifacts SET adopted_head_revision_id=? WHERE id=?",
            params![revision_id, artifact_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn adaptation_completion_rejects_non_boundary_evidence_before_ready() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-evidence-boundary-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, chapter, source) = seed_adaptation_analysis_source(conn)?;
            let started = start_adaptation_analysis_inner(
                conn,
                AdaptationAnalysisStartInput {
                    project_id: "project-analysis".into(),
                    novel_work_id: work.id.clone(),
                    comic_adaptation_id: adaptation.id,
                    comic_adaptation_chapter_id: Some(chapter.clone()),
                    source_analysis_run_id: Some(source),
                    source_artifact_revision_ids: vec![],
                    novel_chapter_revision_id: Some("analysis-revision".into()),
                    base_canon_version_id: work.published_canon_version_id.clone(),
                    base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()),
                    base_continuity_version_id: adaptation
                        .current_continuity_version_id
                        .clone()
                        .ok_or("C0")?,
                    provider_id: None,
                    model_id: None,
                    comic_plan_intent: None,
                    idempotency_key: "non-boundary-completion".into(),
                },
                true,
                "configured_llm".into(),
                "model".into(),
                "owner",
            )?;
            let result = finish_adaptation_analysis_dispatch(
                conn,
                &started.run.id,
                "owner",
                Ok(vec![
                    ("adaptation_proposal".into(), json!({})),
                    (
                        "comic_chapter_plan".into(),
                        json!({"chapters":[{"stableKey":"chapter","sourceSelections":[{"novelChapterRevisionId":"analysis-revision","startUtf8Byte":1,"endUtf8Byte":"正文".len(),"confidence":1.0}],"goal":"目标","turn":"转折","hook":"钩子","pageBudget":1}]}),
                    ),
                    ("scene_plan".into(), json!({})),
                    ("page_panel_plan".into(), json!({})),
                ]),
            );
            let failed = result?;
            assert_eq!(failed.status, "error");
            assert_eq!(failed.safe_error_code.as_deref(), Some("EVIDENCE_RANGE_INVALID"));
            assert_eq!(
                failed.safe_user_message.as_deref(),
                Some("改编引用不是有效的 UTF-8 字节范围，未生成漫画页。")
            );
            assert_eq!(
                conn.query_row::<String, _, _>(
                    "SELECT status FROM adaptation_analysis_runs WHERE id=?",
                    params![started.run.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
                "error"
            );
            assert_eq!(
                conn.query_row::<String, _, _>(
                    "SELECT status FROM adaptation_analysis_run_attempts WHERE adaptation_analysis_run_id=? AND attempt_no=1",
                    params![started.run.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
                "error"
            );
            assert_eq!(
                conn.query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM analysis_artifacts WHERE adaptation_analysis_run_id=?",
                    params![started.run.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
                0
            );
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_accept_uses_utf8_boundaries_and_reuses_preview() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-adaptation-accept-utf8-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let (work, adaptation, _chapter, _source) = seed_adaptation_analysis_source(conn)?;
            let text = "你🙂好";
            let end = text.len() as i64;
            conn.execute(
                "UPDATE novel_chapter_revisions SET content=? WHERE id='analysis-revision'",
                params![text],
            )
            .map_err(|error| error.to_string())?;
            insert_adopted_artifact(
                conn,
                &work.id,
                &adaptation.id,
                "utf8-proposal",
                "utf8-proposal-revision",
                "adaptation_proposal",
                json!({"decisions":[]}),
            )?;
            let valid_plan = json!({"chapters":[{"stableKey":"chapter","sourceSelections":[{"novelChapterRevisionId":"analysis-revision","startUtf8Byte":0,"endUtf8Byte":end,"confidence":1.0}],"goal":"目标","turn":"转折","hook":"钩子","pageBudget":1}]});
            insert_adopted_artifact(
                conn,
                &work.id,
                &adaptation.id,
                "utf8-chapter",
                "utf8-chapter-revision",
                "comic_chapter_plan",
                valid_plan.clone(),
            )?;
            insert_adopted_artifact(
                conn,
                &work.id,
                &adaptation.id,
                "utf8-scene",
                "utf8-scene-revision",
                "scene_plan",
                json!({"comicChapterDraftId":"analysis-adaptation-chapter","scenes":[]}),
            )?;
            let preview_input = AdaptationAcceptPreviewInput {
                project_id: "project-analysis".into(),
                novel_work_id: work.id.clone(),
                comic_adaptation_id: adaptation.id.clone(),
                adaptation_proposal_revision_id: "utf8-proposal-revision".into(),
                comic_chapter_plan_revision_id: "utf8-chapter-revision".into(),
                scene_plan_revision_id: "utf8-scene-revision".into(),
                base_canon_version_id: Some(work.published_canon_version_id.clone()),
                base_continuity_version_id: adaptation.current_continuity_version_id.clone(),
                expected_adaptation_version: Some(0),
                idempotency_key: "utf8-accept".into(),
            };
            let preview = accept_preview_inner(conn, preview_input.clone())?;
            let replay = accept_preview_inner(conn, preview_input)?;
            assert_eq!(replay.operation_id, preview.operation_id);
            let accepted = accept_inner(
                conn,
                AdaptationAcceptInput {
                    project_id: "project-analysis".into(),
                    novel_work_id: work.id.clone(),
                    comic_adaptation_id: adaptation.id.clone(),
                    operation_id: preview.operation_id.clone(),
                    approval_token: replay.approval_token,
                    idempotency_key: "utf8-accept".into(),
                },
            )?;
            assert_eq!(accepted.status, "succeeded");
            assert_eq!(
                conn.query_row::<i64, _, _>(
                    "SELECT source_end FROM comic_planning_chapter_sources WHERE comic_planning_chapter_id=?",
                    params![accepted.entity_map[0].entity_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
                end
            );

            let invalid_plan = |revision: &str, start: i64, finish: i64| {
                json!({"chapters":[{"stableKey":"chapter","sourceSelections":[{"novelChapterRevisionId":revision,"startUtf8Byte":start,"endUtf8Byte":finish,"confidence":1.0}],"goal":"目标","turn":"转折","hook":"钩子","pageBudget":1}]})
            };
            assert_eq!(
                validate_comic_chapter_plan_evidence_ranges(
                    conn,
                    &work.id,
                    &invalid_plan("analysis-revision", 1, end),
                )
                .unwrap_err(),
                "EVIDENCE_RANGE_INVALID"
            );
            assert_eq!(
                validate_comic_chapter_plan_evidence_ranges(
                    conn,
                    &work.id,
                    &invalid_plan("analysis-revision", 0, end + 1),
                )
                .unwrap_err(),
                "EVIDENCE_RANGE_INVALID"
            );
            let foreign = novel_work_create_inner(
                conn,
                NovelWorkCreateInput {
                    project_id: "project-analysis".into(),
                    title: "另一小说".into(),
                    description: None,
                    idempotency_key: "utf8-foreign-work".into(),
                },
            )?;
            conn.execute(
                "INSERT INTO novel_chapters(id,novel_work_id,volume_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES ('utf8-foreign-chapter',?,NULL,1,1,'外章','utf8-foreign-revision',1,1)",
                params![foreign.id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,asset_id,requested_parent_context_revision_id,source_kind,created_at) VALUES ('utf8-foreign-revision','utf8-foreign-chapter',1,?,'hash',NULL,NULL,'paste',1)",
                params![text],
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                validate_comic_chapter_plan_evidence_ranges(
                    conn,
                    &work.id,
                    &invalid_plan("utf8-foreign-revision", 0, end),
                )
                .unwrap_err(),
                "EVIDENCE_SOURCE_SCOPE_INVALID"
            );
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adaptation_create_is_idempotent_scoped_and_initializes_c0() {
        let dir =
            std::env::temp_dir().join(format!("image-client-adaptation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let work = novel_work_create_inner(conn, NovelWorkCreateInput {
                project_id: "project-a".into(), title: "小说 A".into(), description: None, idempotency_key: "work-a".into(),
            })?;
            let input = AdaptationCreateInput { project_id: "project-a".into(), novel_work_id: work.id.clone(), title: "分支 A".into(), idempotency_key: "adaptation-a".into() };
            let created = novel_adaptation_create_inner(conn, input.clone())?;
            assert_eq!(created.id, novel_adaptation_create_inner(conn, input)?.id);
            let continuity_id = created.current_continuity_version_id.as_deref().ok_or("missing C0")?;
            let continuity: (i64, String) = conn.query_row("SELECT version,body_json FROM continuity_state_versions WHERE id=? AND comic_adaptation_id=?",params![continuity_id,created.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
            assert_eq!(continuity, (0, "{}".into()));
            assert!(novel_adaptation_create_inner(conn, AdaptationCreateInput { project_id: "project-b".into(), novel_work_id: work.id, title: "越权".into(), idempotency_key: "foreign".into() }).is_err());
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scene_context_is_explicitly_scoped_approved_and_receipted() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-scene-context-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        db::with_connection(&state, |conn| {
            let work = novel_work_create_inner(conn, NovelWorkCreateInput { project_id:"project-a".into(),title:"小说".into(),description:None,idempotency_key:"work".into() })?;
            let adaptation=novel_adaptation_create_inner(conn,AdaptationCreateInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),title:"改编".into(),idempotency_key:"adaptation".into()})?;
            conn.execute("INSERT INTO novel_chapters (id,novel_work_id,volume_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES ('chapter-1',?,NULL,1,1,'第一章','revision-1',1,1)",params![work.id]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO novel_chapter_revisions (id,novel_chapter_id,version,content,content_hash,asset_id,requested_parent_context_revision_id,source_kind,created_at) VALUES ('revision-1','chapter-1',1,'abcdef','hash',NULL,NULL,'paste',1)",[]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO comic_adaptation_chapters (id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES ('comic-chapter-1',?,'revision-1',1,1)",params![adaptation.id]).map_err(|e|e.to_string())?;
            insert_adopted_artifact(conn,&work.id,&adaptation.id,"proposal","proposal-r","adaptation_proposal",json!({"decisions":[]}))?;
            insert_adopted_artifact(conn,&work.id,&adaptation.id,"chapter","chapter-r","comic_chapter_plan",json!({"chapters":[{"stableKey":"plan-1","sourceSelections":[],"goal":"测试章节","turn":"测试转折","hook":"测试钩子","pageBudget":2}]}))?;
            insert_adopted_artifact(conn,&work.id,&adaptation.id,"scene","scene-r","scene_plan",json!({"comicChapterDraftId":"draft","scenes":[{"stableKey":"scene-1"}]}))?;
            let ts=now();
            conn.execute("INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,expected_adaptation_version,idempotency_key,preview_fingerprint,approval_token_hash,approval_expires_at,status,created_at,updated_at,completed_at) VALUES ('accept-op','accept_adaptation',NULL,?,NULL,0,'accept-key','fp','hash',?, 'succeeded',?,?,?)",params![adaptation.id,ts+1_000,ts,ts,ts]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES ('head',?,'proposal-r','chapter-r','scene-r','accept-op','active',?,?)",params![adaptation.id,ts,ts]).map_err(|e|e.to_string())?;
            let wrong_head_error=scene_context_resolve_inner(conn,SceneContextResolveInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),scene_plan_revision_id:"scene-r".into(),planning_scene_stable_key:"scene-1".into(),working_context_revision_id:None,canon_version_id:work.published_canon_version_id.clone(),novel_state_version_id:work.current_novel_state_version_id.clone(),continuity_version_id:adaptation.current_continuity_version_id.clone(),adaptation_plan_revision_id:"head".into(),selected_entity_ids:vec![],visual_card_revision_ids:vec![],idempotency_key:"resolve-wrong-head".into()}).err().unwrap();
            assert_eq!(wrong_head_error,"REVISION_CONFLICT","a plan-head id is not an adopted adaptation-proposal revision");
            let resolved=scene_context_resolve_inner(conn,SceneContextResolveInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),scene_plan_revision_id:"scene-r".into(),planning_scene_stable_key:"scene-1".into(),working_context_revision_id:None,canon_version_id:work.published_canon_version_id.clone(),novel_state_version_id:work.current_novel_state_version_id.clone(),continuity_version_id:adaptation.current_continuity_version_id.clone(),adaptation_plan_revision_id:"proposal-r".into(),selected_entity_ids:vec![],visual_card_revision_ids:vec![],idempotency_key:"resolve".into()})?;
            assert_eq!(resolved.status,"provisional");
            assert_eq!(resolved.adaptation_plan_revision_id,"proposal-r");
            let approved=scene_context_approve_inner(conn,SceneContextApproveInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),scene_context_snapshot_id:resolved.snapshot_id.clone(),context_fingerprint:resolved.context_fingerprint.clone(),idempotency_key:"approve".into()})?;
            assert_eq!(approved.status,"approved");
            conn.execute("INSERT INTO comic_planning_chapters (id,comic_adaptation_plan_head_id,comic_adaptation_id,planning_chapter_stable_key,status,created_at,updated_at) VALUES ('planning-1','head',?,'plan-1','planning',1,1)",params![adaptation.id]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-1','revision-1',0,0,1)",[]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO analysis_artifacts (id,adaptation_analysis_run_id,artifact_type,novel_work_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES ('page','fixture-analysis-run','page_panel_plan',?,?,?,'active',0,1,1)",params![work.id,adaptation.id,"comic-chapter-1"]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES ('page-r','page',1,NULL,?,'','fixture','{}','{}','adopted',1)",params![r#"{"comicChapterDraftId":"comic-chapter-1","pages":[{"stableKey":"page-1","pageNo":1,"layout":{"templateId":"reveal_focus_4","panelCount":4,"readingOrder":[1,2,3,4],"dominantPanel":4,"geometry":{"coordinateSystem":"normalized-0-1","panelCount":4,"readingOrder":[1,2,3,4],"gutter":0.04,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[{"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.48,"y":0.04},{"x":0.48,"y":0.48},{"x":0.04,"y":0.48}],"bounds":{"x":0.04,"y":0.04,"width":0.44,"height":0.44},"textZone":{"x":0.1,"y":0.1,"width":0.1,"height":0.1},"bleed":{"top":true,"left":true}},{"panelNo":2,"polygon":[{"x":0.52,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.48},{"x":0.52,"y":0.48}],"bounds":{"x":0.52,"y":0.04,"width":0.44,"height":0.44},"textZone":{"x":0.6,"y":0.1,"width":0.1,"height":0.1},"bleed":{"top":true,"right":true}},{"panelNo":3,"polygon":[{"x":0.04,"y":0.52},{"x":0.48,"y":0.52},{"x":0.48,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.52,"width":0.44,"height":0.44},"textZone":{"x":0.1,"y":0.6,"width":0.1,"height":0.1},"bleed":{"bottom":true,"left":true}},{"panelNo":4,"polygon":[{"x":0.52,"y":0.52},{"x":0.96,"y":0.52},{"x":0.96,"y":0.96},{"x":0.52,"y":0.96}],"bounds":{"x":0.52,"y":0.52,"width":0.44,"height":0.44},"textZone":{"x":0.6,"y":0.6,"width":0.1,"height":0.1},"bleed":{"bottom":true,"right":true}}]}},"panels":[{"stableKey":"panel-1","panelNo":1,"planningSceneStableKey":"scene-1"},{"stableKey":"panel-2","panelNo":2,"planningSceneStableKey":"scene-1"},{"stableKey":"panel-3","panelNo":3,"planningSceneStableKey":"scene-1"},{"stableKey":"panel-4","panelNo":4,"planningSceneStableKey":"scene-1"}]}]}"#]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE analysis_artifact_revisions SET body_json=json_remove(body_json,'$.pages[0].layout.geometry.panels[0].bleed','$.pages[0].layout.geometry.panels[1].bleed','$.pages[0].layout.geometry.panels[2].bleed','$.pages[0].layout.geometry.panels[3].bleed') WHERE id='page-r'",[]).map_err(|e|e.to_string())?;
            // The production fixture uses the real v1 five-panel template
            // shape.  It intentionally has irregular upper/bottom splits,
            // rather than treating a regular grid as the only comic layout.
            let five_geometry=json!({"coordinateSystem":"normalized-0-1","panelCount":5,"readingOrder":[1,2,3,4,5],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[
                {"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.59,"y":0.04},{"x":0.54,"y":0.25},{"x":0.04,"y":0.25}],"bounds":{"x":0.04,"y":0.04,"width":0.55,"height":0.21},"textZone":{"x":0.08,"y":0.08,"width":0.34,"height":0.1}},
                {"panelNo":2,"polygon":[{"x":0.61,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.25},{"x":0.56,"y":0.25}],"bounds":{"x":0.56,"y":0.04,"width":0.40,"height":0.21},"textZone":{"x":0.67,"y":0.08,"width":0.2,"height":0.1}},
                {"panelNo":3,"polygon":[{"x":0.04,"y":0.27},{"x":0.96,"y":0.27},{"x":0.96,"y":0.66},{"x":0.04,"y":0.66}],"bounds":{"x":0.04,"y":0.27,"width":0.92,"height":0.39},"textZone":{"x":0.16,"y":0.37,"width":0.62,"height":0.12}},
                {"panelNo":4,"polygon":[{"x":0.04,"y":0.68},{"x":0.43,"y":0.68},{"x":0.43,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.68,"width":0.39,"height":0.28},"textZone":{"x":0.08,"y":0.75,"width":0.23,"height":0.1}},
                {"panelNo":5,"polygon":[{"x":0.45,"y":0.68},{"x":0.96,"y":0.68},{"x":0.96,"y":0.96},{"x":0.45,"y":0.96}],"bounds":{"x":0.45,"y":0.68,"width":0.51,"height":0.28},"textZone":{"x":0.57,"y":0.75,"width":0.28,"height":0.1}}
            ]});
            let five_layout=json!({"schemaVersion":1,"templateId":"hero_middle_5","layoutKind":"template","source":"auto","panelCount":5,"readingOrder":[1,2,3,4,5],"dominantPanel":3,"rationale":"中部主视觉","instructions":["顶部不等宽轻斜切双格","中部通栏高光","底部不等宽双格"],"targetPanelMappings":[{"targetPanelNo":1,"sourcePanelNos":[1],"mode":"one_to_one","isDominant":false},{"targetPanelNo":2,"sourcePanelNos":[2],"mode":"one_to_one","isDominant":false},{"targetPanelNo":3,"sourcePanelNos":[3],"mode":"one_to_one","isDominant":true},{"targetPanelNo":4,"sourcePanelNos":[4],"mode":"one_to_one","isDominant":false},{"targetPanelNo":5,"sourcePanelNos":[5],"mode":"one_to_one","isDominant":false}],"geometry":five_geometry,"geometrySource":"template","dominantPanelProvenance":"template"});
            let five_panels=(1..=5).map(|panel_no| json!({"stableKey":format!("panel-{panel_no}"),"panelNo":panel_no,"planningSceneStableKey":"scene-1","action":format!("节拍 {panel_no}"),"dialogues":[{"speaker":"主角","text":format!("对白 {panel_no}")}]})).collect::<Vec<_>>();
            let second_five_panels=(1..=5).map(|panel_no| json!({"stableKey":format!("page-2-panel-{panel_no}"),"panelNo":panel_no,"planningSceneStableKey":"scene-1","action":format!("第二页节拍 {panel_no}"),"dialogues":[{"speaker":"主角","text":format!("第二页对白 {panel_no}")}]})).collect::<Vec<_>>();
            let five_plan=json!({"comicChapterDraftId":"comic-chapter-1","pages":[
                {"stableKey":"page-1","pageNo":1,"layout":five_layout.clone(),"panels":five_panels},
                {"stableKey":"page-2","pageNo":2,"layout":five_layout,"panels":second_five_panels}
            ]});
            conn.execute("UPDATE analysis_artifact_revisions SET body_json=? WHERE id='page-r'",params![serde_json::to_string(&five_plan).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE analysis_artifacts SET adopted_head_revision_id='page-r' WHERE id='page'",[]).map_err(|e|e.to_string())?;
            let preview=comic_plan_apply_preview_inner(conn,ComicPlanApplyPreviewInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),accepted_plan_version_id:"head".into(),page_panel_plan_revision_id:"page-r".into(),scene_context_selections:vec![ComicPlanSelection{planning_scene_stable_key:"scene-1".into(),scene_context_snapshot_id:resolved.snapshot_id.clone()}],base_canon_version_id:work.published_canon_version_id.clone(),base_continuity_version_id:adaptation.current_continuity_version_id.clone().ok_or("missing C0")?,expected_adaptation_version:0,idempotency_key:"apply-preview".into()})?;
            let applied=comic_plan_apply_inner(conn,ComicPlanApplyInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),operation_id:preview.operation_id.clone(),approval_token:preview.approval_token,idempotency_key:"apply-preview".into()})?;
            assert_eq!(applied.entity_map.len(),14);
            assert_eq!(conn.query_row::<i64,_,_>("SELECT optimistic_version FROM comic_adaptations WHERE id=?",params![adaptation.id],|r|r.get(0)).map_err(|e|e.to_string())?,1);
            assert!(resolved.resolved_context["canon"]["body"].is_object());
            let production_chapter = applied.entity_map.iter().find(|entry| entry.entity_kind == "production_chapter").ok_or("missing production chapter")?.entity_id.clone();
            let visual = crate::comic_visual::prepare_inner(conn, crate::comic_visual::ComicVisualManifestPrepareInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(),
                apply_operation_id: preview.operation_id.clone(), production_chapter_id: production_chapter,
                idempotency_key: "visual-manifest".into(),
            })?;
            assert_eq!(visual.freshness, "ready");
            assert_eq!(visual.manifest["pages"].as_array().map(Vec::len), Some(2));
            assert_eq!(visual.manifest["pages"][0]["panels"].as_array().map(Vec::len), Some(5));
            assert_eq!(visual.manifest["pages"][0]["layout"]["templateId"], "hero_middle_5");
            assert_eq!(visual.manifest["provenance"]["sourceRevisionIds"], json!(["revision-1"]));
            assert_eq!(visual.manifest["provenance"]["sceneContexts"][0]["resolvedContext"], resolved.resolved_context);
            assert_eq!(visual.manifest["provenance"]["sceneContexts"][0]["resolvedContextHash"], resolved.context_fingerprint);
            assert_eq!(visual.manifest["pages"][0]["panels"][4]["spec"]["dialogues"][0]["text"], "对白 5");
            let visual_authorization = crate::comic_visual_batch::freeze_authorization(
                &crate::comic_visual_batch::VisualOutputInput {
                    target: "comic_pages".into(),
                    image_options: Some(crate::comic_visual_batch::VisualImageOptionsInput {
                        model: Some("visual-test-model".into()),
                        size: Some("1024x1536".into()),
                    }),
                },
                "visual-test-provider",
                "visual-test-model",
            )?;
            let old_visual_job = crate::novel::test_create_succeeded_visual_job(
                conn, "project-a", &work.id, "chapter-1", "revision-1", &adaptation.id,
                &preview.operation_id, None,
            )?;
            crate::comic_visual_batch::assert_real_authorized_two_page_batch(
                conn,
                &old_visual_job,
                &visual_authorization,
                &dir.join("visual-batch-output"),
            )?;
            let visual_batch_id: String = conn
                .query_row(
                    "SELECT id FROM comic_visual_batches WHERE production_job_id=?",
                    params![old_visual_job.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let visual_export_output = dir.join("visual-export-output");
            std::fs::create_dir_all(&visual_export_output).map_err(|e| e.to_string())?;
            crate::comic_visual_export::assert_real_batch_export(
                conn,
                &visual_batch_id,
                "project-a",
                &work.id,
                &dir.join("visual-batch-output"),
                &visual_export_output,
            )?;
            crate::comic_visual_render::assert_real_manifest_page_run(
                conn,
                &visual,
                &dir.join("visual-batch-output"),
            )?;
            // The batch has rendered both ordered pages; the renderer helper
            // above reuses its first durable candidate and exercises only a
            // correctly-parented follow-up reconciliation attempt.
            for entity in &applied.entity_map {
                let found = entity.entity_kind == "production_chapter"
                    && visual.production_chapter_id == entity.entity_id
                    || visual.manifest.to_string().contains(&entity.entity_id);
                assert!(found, "missing production entity {} in manifest", entity.entity_id);
            }
            let reused = crate::comic_visual::prepare_inner(conn, crate::comic_visual::ComicVisualManifestPrepareInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(),
                apply_operation_id: preview.operation_id.clone(), production_chapter_id: visual.production_chapter_id.clone(),
                idempotency_key: "visual-manifest-second-fresh-key".into(),
            })?;
            assert_eq!(reused.id, visual.id);
            assert_eq!(conn.query_row::<i64,_,_>("SELECT COUNT(*) FROM comic_visual_manifests", [], |row| row.get(0)).map_err(|e|e.to_string())?, 1);
            assert_eq!(crate::comic_visual::prepare_inner(conn, crate::comic_visual::ComicVisualManifestPrepareInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(),
                apply_operation_id: preview.operation_id.clone(), production_chapter_id: "other-production-chapter".into(),
                idempotency_key: "visual-manifest".into(),
            }).map(|_| ()).unwrap_err(), "IDEMPOTENCY_MISMATCH");
            assert_eq!(crate::comic_visual::get_inner(conn, crate::comic_visual::ComicVisualManifestGetInput { project_id:"project-b".into(), novel_work_id:work.id.clone(), manifest_id:visual.id.clone() }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_UNKNOWN");
            assert_eq!(crate::comic_visual::get_inner(conn, crate::comic_visual::ComicVisualManifestGetInput { project_id:"project-a".into(), novel_work_id:"other-work".into(), manifest_id:visual.id.clone() }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_UNKNOWN");
            assert_eq!(crate::comic_visual::list_inner(conn, crate::comic_visual::ComicVisualManifestListInput { project_id:"project-b".into(), novel_work_id:work.id.clone(), production_chapter_id:visual.production_chapter_id.clone() }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_SCOPE_MISMATCH");
            assert_eq!(crate::comic_visual::list_inner(conn, crate::comic_visual::ComicVisualManifestListInput { project_id:"project-a".into(), novel_work_id:"other-work".into(), production_chapter_id:visual.production_chapter_id.clone() }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_SCOPE_MISMATCH");
            assert_eq!(crate::comic_visual::list_inner(conn, crate::comic_visual::ComicVisualManifestListInput { project_id:"project-a".into(), novel_work_id:work.id.clone(), production_chapter_id:"other-production-chapter".into() }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_SCOPE_MISMATCH");
            // The same idempotency key must return the immutable record, not a
            // second page plan snapshot.
            assert_eq!(visual.id, crate::comic_visual::prepare_inner(conn, crate::comic_visual::ComicVisualManifestPrepareInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(),
                apply_operation_id: preview.operation_id.clone(), production_chapter_id: visual.production_chapter_id.clone(),
                idempotency_key: "visual-manifest".into(),
            })?.id);
            conn.execute("UPDATE comic_adaptations SET optimistic_version=2 WHERE id=?", params![adaptation.id]).map_err(|e|e.to_string())?;
            let stale = crate::comic_visual::get_inner(conn, crate::comic_visual::ComicVisualManifestGetInput { project_id:"project-a".into(), novel_work_id:work.id.clone(), manifest_id:visual.id.clone() })?;
            assert_eq!(stale.freshness, "stale");
            // The original receipt can be read as history, but a fresh key
            // cannot turn that stale production tree into a new ready request.
            assert_eq!(crate::comic_visual::prepare_inner(conn, crate::comic_visual::ComicVisualManifestPrepareInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), comic_adaptation_id: adaptation.id.clone(),
                apply_operation_id: preview.operation_id.clone(), production_chapter_id: visual.production_chapter_id.clone(),
                idempotency_key: "visual-manifest-new-key-after-stale".into(),
            }).map(|_| ()).unwrap_err(), "VISUAL_MANIFEST_STALE_OR_SCOPE_MISMATCH");
            assert_eq!(comic_plan_apply_inner(conn,ComicPlanApplyInput{project_id:"project-a".into(),novel_work_id:work.id.clone(),comic_adaptation_id:adaptation.id.clone(),operation_id:preview.operation_id,approval_token:"used".into(),idempotency_key:"apply-preview".into()})?.entity_map.len(),14);
            assert!(scene_context_approve_inner(conn,SceneContextApproveInput{project_id:"project-b".into(),novel_work_id:work.id,comic_adaptation_id:adaptation.id,scene_context_snapshot_id:resolved.snapshot_id,context_fingerprint:resolved.context_fingerprint,idempotency_key:"foreign".into()}).is_err());
            Ok(())
        }).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn custom_irregular_layout_is_validated_before_apply_preview() {
        let layout = json!({"templateId":"custom_irregular","layoutKind":"custom_irregular","panelCount":1,"readingOrder":[1],"dominantPanel":1,"geometry":{"coordinateSystem":"normalized-0-1","panelCount":1,"readingOrder":[1],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[{"panelNo":1,"polygon":[{"x":0.0,"y":0.0},{"x":1.0,"y":0.0},{"x":1.0,"y":1.0},{"x":0.0,"y":1.0}],"bounds":{"x":0.0,"y":0.0,"width":1.0,"height":1.0},"textZone":{"x":0.2,"y":0.2,"width":0.2,"height":0.1},"bleed":{"top":true,"right":true,"bottom":true,"left":true}}]}});
        assert!(validate_page_layout(&layout, &[json!({"panelNo":1})]).is_ok());
        let mut invalid = layout;
        invalid["geometry"]["panels"][0]["polygon"][0]["x"] = json!(1.2);
        assert_eq!(
            validate_page_layout(&invalid, &[json!({"panelNo":1})]),
            Err("LAYOUT_INVALID".into())
        );
    }

    #[test]
    fn geometry_relationships_overlap_gutter_parent_and_bleed_fail_closed() {
        let layout = json!({"templateId":"custom_irregular","panelCount":2,"readingOrder":[1,2],"dominantPanel":1,"geometry":{"coordinateSystem":"normalized-0-1","panelCount":2,"readingOrder":[1,2],"gutter":0.04,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[{"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.46,"y":0.04},{"x":0.46,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.04,"width":0.42,"height":0.92},"textZone":{"x":0.1,"y":0.2,"width":0.1,"height":0.1}},{"panelNo":2,"polygon":[{"x":0.54,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.96},{"x":0.54,"y":0.96}],"bounds":{"x":0.54,"y":0.04,"width":0.42,"height":0.92},"textZone":{"x":0.6,"y":0.2,"width":0.1,"height":0.1}}]}});
        let source = [json!({"panelNo":1}), json!({"panelNo":2})];
        assert!(validate_page_layout(&layout, &source).is_ok());
        let mut overlap = layout.clone();
        overlap["geometry"]["panels"][1]["polygon"][0]["x"] = json!(0.4);
        overlap["geometry"]["panels"][1]["polygon"][3]["x"] = json!(0.4);
        overlap["geometry"]["panels"][1]["bounds"]["x"] = json!(0.4);
        overlap["geometry"]["panels"][1]["bounds"]["width"] = json!(0.56);
        assert!(validate_page_layout(&overlap, &source).is_err());
        let mut gutter = layout.clone();
        gutter["geometry"]["panels"][1]["polygon"][0]["x"] = json!(0.47);
        gutter["geometry"]["panels"][1]["polygon"][3]["x"] = json!(0.47);
        gutter["geometry"]["panels"][1]["bounds"]["x"] = json!(0.47);
        gutter["geometry"]["panels"][1]["bounds"]["width"] = json!(0.49);
        assert!(validate_page_layout(&gutter, &source).is_err());
        let mut parent = layout.clone();
        parent["geometry"]["panels"][0]["parentPanelNo"] = json!(2);
        parent["geometry"]["panels"][1]["parentPanelNo"] = json!(1);
        assert!(validate_page_layout(&parent, &source).is_err());
        let mut bleed = layout;
        bleed["geometry"]["panels"][0]["bleed"] = json!({"top":true});
        assert!(validate_page_layout(&bleed, &source).is_err());
    }

    #[test]
    fn chapter_plan_uses_schema_source_selections_ranges() {
        let ranges = chapter_keys_and_ranges(&json!({"chapters":[{"stableKey":"plan-1","sourceSelections":[{"novelChapterRevisionId":"chapter-r","startUtf8Byte":2,"endUtf8Byte":7,"evidenceExcerpt":"片段","confidence":0.9}]}]})).unwrap();
        assert_eq!(
            ranges,
            vec![("plan-1".into(), vec![("chapter-r".into(), 2, 7)])]
        );
    }
}
