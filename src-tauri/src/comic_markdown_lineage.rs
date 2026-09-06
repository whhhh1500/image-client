//! Content receipts and a single, ordered book snapshot. Revision numbers remain CAS tokens.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Entry {
    hash: Option<String>,
    label: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    version: u8,
    values: BTreeMap<String, Entry>,
}
#[derive(Clone)]
struct Chapter {
    id: String,
    sequence: i64,
    number: i64,
    title: Option<String>,
    source_id: Option<String>,
    content: String,
}
#[derive(Clone)]
struct Raw {
    document: Document,
    chapter: String,
    receipt: String,
    state_text: String,
    state_hash: String,
    supplement: String,
    supplement_hash: String,
}
#[derive(Clone)]
struct Historic {
    revision: i64,
    markdown: String,
    receipt: String,
    time: i64,
    state_hash: String,
    supplement_hash: String,
}
pub(super) struct Book {
    scope: Scope,
    chapters: Vec<Chapter>,
    raw: Vec<Raw>,
    history: BTreeMap<String, Vec<Historic>>,
    sources: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTarget {
    pub document_id: String,
    pub revision: i64,
    pub kind: String,
    pub page_no: Option<i64>,
    pub reasons: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub fingerprint: String,
    pub targets: Vec<SyncTarget>,
    pub missing_page_nos: Vec<i64>,
    pub obsolete_page_nos: Vec<i64>,
    pub blocked_reason: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedChapter {
    chapter_id: String,
    chapter_no: i64,
    title: Option<String>,
    document_count: usize,
    reason: String,
}
fn rank(kind: &str) -> u8 {
    match kind {
        "settings" => 0,
        "script" => 1,
        "storyboard" => 2,
        _ => 3,
    }
}
fn state(md: &str) -> String {
    ["人物锚点", "人物锚点补充", "连续性要求"]
        .iter()
        .map(|name| format!("### {name}\n{}", section(md, name).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n\n")
}
impl Book {
    pub(super) fn load(c: &Connection, s: &Scope) -> Result<Self, String> {
        source(c, s)?;
        let mut st=c.prepare("SELECT ch.id,ch.sequence_no,ch.chapter_no,ch.title,ch.current_revision_id,COALESCE(r.content,'') FROM novel_chapters ch LEFT JOIN novel_chapter_revisions r ON r.id=ch.current_revision_id AND r.novel_chapter_id=ch.id WHERE ch.novel_work_id=? ORDER BY ch.sequence_no,ch.id").map_err(sql)?;
        let chapters = st
            .query_map([&s.novel_work_id], |r| {
                Ok(Chapter {
                    id: r.get(0)?,
                    sequence: r.get(1)?,
                    number: r.get(2)?,
                    title: r.get(3)?,
                    source_id: r.get(4)?,
                    content: r.get(5)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        let mut st=c.prepare("SELECT id,chapter_id,kind,page_no,markdown,revision,dependencies,updated_at,optimization_instruction FROM comic_md_documents WHERE novel_work_id=? AND project_id=?").map_err(sql)?;
        let mut raw = st
            .query_map(params![s.novel_work_id, s.project_id], |r| {
                let markdown: String = r.get(4)?;
                let kind: String = r.get(2)?;
                let page: i64 = r.get(3)?;
                let state_text = state(&markdown);
                let state_hash = hash(&state_text);
                let supplement = section(&markdown, "人物锚点补充").unwrap_or_default();
                let supplement_hash = hash(&supplement);
                Ok(Raw {
                    state_text,
                    state_hash,
                    supplement,
                    supplement_hash,
                    chapter: r.get(1)?,
                    receipt: r.get(6)?,
                    document: Document {
                        id: r.get(0)?,
                        kind: kind.clone(),
                        page_no: if page == 0 { None } else { Some(page) },
                        content_hash: hash(&markdown),
                        issues: validate(
                            &kind,
                            if page == 0 { None } else { Some(page) },
                            &markdown,
                        ),
                        markdown,
                        optimization_instruction: r.get(8)?,
                        revision: r.get(5)?,
                        stale: false,
                        stale_reasons: vec![],
                        out_of_plan: false,
                        updated_at: r.get(7)?,
                    },
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        raw.sort_by_key(|d| {
            (
                if d.document.kind == "settings" {
                    i64::MIN
                } else {
                    chapters
                        .iter()
                        .find(|ch| ch.id == d.chapter)
                        .map_or(i64::MAX, |ch| ch.sequence)
                },
                rank(&d.document.kind),
                d.document.page_no,
            )
        });
        let mut history: BTreeMap<String, Vec<Historic>> = BTreeMap::new();
        let legacy = raw
            .iter()
            .any(|r| serde_json::from_str::<Receipt>(&r.receipt).is_err());
        if legacy {
            let mut st=c.prepare("SELECT r.document_id,r.revision,r.markdown,r.dependencies,r.created_at FROM comic_md_revisions r JOIN comic_md_documents d ON d.id=r.document_id WHERE d.novel_work_id=? AND d.project_id=? ORDER BY r.revision").map_err(sql)?;
            for row in st
                .query_map(params![s.novel_work_id, s.project_id], |r| {
                    let markdown: String = r.get(2)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        Historic {
                            revision: r.get(1)?,
                            state_hash: hash(&state(&markdown)),
                            supplement_hash: hash(
                                &section(&markdown, "人物锚点补充").unwrap_or_default(),
                            ),
                            markdown,
                            receipt: r.get(3)?,
                            time: r.get(4)?,
                        },
                    ))
                })
                .map_err(sql)?
            {
                let (id, h) = row.map_err(sql)?;
                history.entry(id).or_default().push(h);
            }
        }
        let mut sources = BTreeMap::new();
        if legacy {
            let mut st=c.prepare("SELECT r.id,r.content FROM novel_chapter_revisions r JOIN novel_chapters ch ON ch.id=r.novel_chapter_id WHERE ch.novel_work_id=?").map_err(sql)?;
            sources = st
                .query_map([&s.novel_work_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql)?
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map_err(sql)?;
        }
        let mut b = Self {
            scope: s.clone(),
            chapters,
            raw,
            history,
            sources,
        };
        for i in 0..b.raw.len() {
            let r = b.raw[i].clone();
            let d = &r.document;
            let out = d.kind == "page_prompt"
                && b.page_plan(&r.chapter)
                    .is_some_and(|p| !p.contains(&d.page_no.unwrap_or(0)));
            let current = b.receipt(&r.chapter, &d.kind, d.page_no);
            let old = b.decode(&r);
            let mut reasons = vec![];
            if let Some(old) = old {
                for key in old.values.keys().chain(current.values.keys()) {
                    if old.values.get(key) != current.values.get(key) {
                        let label = current
                            .values
                            .get(key)
                            .or(old.values.get(key))
                            .map(|e| e.label.as_str())
                            .unwrap_or("关联资料");
                        let reason = format!("{label}已变化，可能受影响，请同步或核对");
                        if !reasons.contains(&reason) {
                            reasons.push(reason);
                        }
                    }
                }
            } else {
                reasons.push("历史关联资料不可核验，请同步或核对".into());
            }
            for upstream in b.upstream(&r.chapter, &d.kind, d.page_no) {
                if upstream.document.stale {
                    let reason = format!(
                        "{}尚未更新，可能受影响，请先同步或核对上游",
                        document_label(&upstream.document)
                    );
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
            b.raw[i].document.stale = !reasons.is_empty();
            b.raw[i].document.stale_reasons = reasons;
            b.raw[i].document.out_of_plan = out;
        }
        Ok(b)
    }
    fn sequence(&self, ch: &str) -> i64 {
        self.chapters
            .iter()
            .find(|c| c.id == ch)
            .map_or(i64::MIN, |c| c.sequence)
    }
    fn page_plan(&self, ch: &str) -> Option<Vec<i64>> {
        self.raw
            .iter()
            .find(|r| {
                r.chapter == ch && r.document.kind == "storyboard" && r.document.issues.is_empty()
            })
            .map(|r| {
                pages(&r.document.markdown)
                    .into_iter()
                    .map(|(n, _)| n)
                    .collect()
            })
    }
    fn upstream(&self, ch: &str, kind: &str, page: Option<i64>) -> Vec<&Raw> {
        if kind == "settings" {
            return vec![];
        }
        self.raw
            .iter()
            .filter(|r| {
                let d = &r.document;
                d.kind == "settings"
                    || (r.chapter == ch && rank(&d.kind) < rank(kind))
                    || (self.sequence(&r.chapter) < self.sequence(ch) && d.kind == "script")
                    || (d.kind == "page_prompt"
                        && !d.out_of_plan
                        && ((self.sequence(&r.chapter) < self.sequence(ch))
                            || (kind == "page_prompt" && r.chapter == ch && d.page_no < page)))
            })
            .collect()
    }
    fn receipt(&self, ch: &str, kind: &str, page: Option<i64>) -> Receipt {
        let mut values = BTreeMap::new();
        if kind == "settings" {
            return Receipt { version: 2, values };
        }
        for chapter in &self.chapters {
            if chapter.id == ch {
                values.insert(
                    format!("source:{}", chapter.id),
                    Entry {
                        hash: chapter.source_id.as_ref().map(|_| hash(&chapter.content)),
                        label: if chapter.id == ch {
                            "章节正文".into()
                        } else {
                            format!("第{}章正文", chapter.number)
                        },
                    },
                );
            }
        }
        for k in ["settings", "script", "storyboard"] {
            if rank(k) >= rank(kind) {
                break;
            }
            let key = if k == "settings" { "" } else { ch };
            let d = self
                .raw
                .iter()
                .find(|r| r.chapter == key && r.document.kind == k);
            values.insert(
                format!("doc:{key}:{k}"),
                Entry {
                    hash: d.map(|r| r.document.content_hash.clone()),
                    label: match k {
                        "settings" => "作品设定",
                        "script" => "本章剧本",
                        _ => "分页分镜",
                    }
                    .into(),
                },
            );
        }
        for chapter in &self.chapters {
            if chapter.sequence < self.sequence(ch) {
                let d = self
                    .raw
                    .iter()
                    .find(|r| r.chapter == chapter.id && r.document.kind == "script");
                values.insert(
                    format!("doc:{}:script", chapter.id),
                    Entry {
                        hash: d.map(|r| r.supplement_hash.clone()),
                        label: format!("第{}章剧本", chapter.number),
                    },
                );
            }
        }
        if kind != "settings" {
            for r in self
                .upstream(ch, kind, page)
                .into_iter()
                .filter(|r| r.document.kind == "page_prompt")
            {
                values.insert(
                    format!("state:{}", r.document.id),
                    Entry {
                        hash: Some(r.state_hash.clone()),
                        label: format!(
                            "第{}章第{}页人物状态",
                            self.chapters
                                .iter()
                                .find(|c| c.id == r.chapter)
                                .map_or(0, |c| c.number),
                            r.document.page_no.unwrap_or(0)
                        ),
                    },
                );
            }
        }
        Receipt { version: 2, values }
    }
    fn decode(&self, r: &Raw) -> Option<Receipt> {
        if let Ok(receipt) = serde_json::from_str::<Receipt>(&r.receipt) {
            return (receipt.version == 2).then_some(receipt);
        }
        let legacy: Vec<serde_json::Value> = serde_json::from_str(&r.receipt).ok()?;
        let mut old = self.receipt(&r.chapter, &r.document.kind, r.document.page_no);
        old.values.clear();
        if r.document.kind == "settings" {
            return Some(old);
        }
        let current = self.receipt(&r.chapter, &r.document.kind, r.document.page_no);
        for item in legacy {
            let a = item.as_array()?;
            let key = a.first()?.as_str()?;
            if key == "source" {
                let value = a.get(1)?;
                let h = if value.is_null() {
                    None
                } else {
                    Some(hash(self.sources.get(value.as_str()?)?))
                };
                let key = format!("source:{}", r.chapter);
                old.values.insert(
                    key.clone(),
                    Entry {
                        hash: h,
                        label: current.values.get(&key)?.label.clone(),
                    },
                );
            } else if ["settings", "script", "storyboard"].contains(&key) {
                let value = a.get(1)?;
                let h = if value.is_null() {
                    None
                } else {
                    Some(self.historical_hash(value.get(0)?.as_str()?, value.get(1)?.as_i64()?)?)
                };
                let ch = if key == "settings" { "" } else { &r.chapter };
                let key = format!("doc:{ch}:{key}");
                old.values.insert(
                    key.clone(),
                    Entry {
                        hash: h,
                        label: current.values.get(&key)?.label.clone(),
                    },
                );
            } else {
                let h = if a.get(2)?.is_null() {
                    None
                } else {
                    let rev = a.get(3)?.as_i64()?;
                    let historic = self
                        .history
                        .get(a.get(2)?.as_str()?)?
                        .iter()
                        .find(|h| h.revision == rev)?;
                    Some(historic.supplement_hash.clone())
                };
                let dk = format!("doc:{key}:script");
                old.values.insert(
                    dk.clone(),
                    Entry {
                        hash: h,
                        label: current.values.get(&dk)?.label.clone(),
                    },
                );
            }
        }
        // Legacy receipts had no page-state entries. Recover them at the saved content's
        // original timestamp, never from today's content (which would erase changes).
        if r.document.kind != "settings" {
            let time = self
                .history
                .get(&r.document.id)?
                .iter()
                .find(|h| h.markdown == r.document.markdown && h.receipt == r.receipt)?
                .time;
            for upstream in self.raw.iter().filter(|u| {
                u.document.kind == "page_prompt"
                    && (self.sequence(&u.chapter) < self.sequence(&r.chapter)
                        || (r.document.kind == "page_prompt"
                            && u.chapter == r.chapter
                            && u.document.page_no < r.document.page_no))
            }) {
                if let Some(h) = self
                    .history
                    .get(&upstream.document.id)?
                    .iter()
                    .rev()
                    .find(|h| h.time <= time)
                {
                    if self
                        .history
                        .get(&upstream.document.id)?
                        .iter()
                        .any(|other| other.time == h.time && other.state_hash != h.state_hash)
                    {
                        return None;
                    }
                    if let Some(board) = self
                        .raw
                        .iter()
                        .find(|b| b.chapter == upstream.chapter && b.document.kind == "storyboard")
                    {
                        if let Some(hb) = self
                            .history
                            .get(&board.document.id)?
                            .iter()
                            .rev()
                            .find(|h| h.time <= time)
                        {
                            if self
                                .history
                                .get(&board.document.id)?
                                .iter()
                                .any(|other| other.time == hb.time && other.markdown != hb.markdown)
                            {
                                return None;
                            }
                            if validate("storyboard", None, &hb.markdown).is_empty()
                                && !pages(&hb.markdown)
                                    .iter()
                                    .any(|(p, _)| Some(*p) == upstream.document.page_no)
                            {
                                continue;
                            }
                        }
                    }
                    let key = format!("state:{}", upstream.document.id);
                    let label = format!(
                        "第{}章第{}页人物状态",
                        self.chapters
                            .iter()
                            .find(|c| c.id == upstream.chapter)
                            .map_or(0, |c| c.number),
                        upstream.document.page_no.unwrap_or(0)
                    );
                    old.values.insert(
                        key,
                        Entry {
                            hash: Some(h.state_hash.clone()),
                            label,
                        },
                    );
                }
            }
        }
        Some(old)
    }
    fn historical_hash(&self, id: &str, rev: i64) -> Option<String> {
        self.history
            .get(id)?
            .iter()
            .find(|h| h.revision == rev)
            .map(|h| hash(&h.markdown))
    }
    pub(super) fn documents(&self) -> Vec<Document> {
        self.raw
            .iter()
            .filter(|r| r.chapter == self.scope.chapter_id || r.document.kind == "settings")
            .map(|r| r.document.clone())
            .collect()
    }
    pub(super) fn dependencies(&self, kind: &str, page: Option<i64>) -> String {
        serde_json::to_string(&self.receipt(&self.scope.chapter_id, kind, page)).unwrap()
    }
    pub(super) fn guard(&self) -> String {
        let current = self.chapters.iter().find(|c| c.id == self.scope.chapter_id);
        let local = self
            .raw
            .iter()
            .filter(|r| r.chapter == self.scope.chapter_id)
            .map(|r| (&r.document.id, r.document.revision))
            .collect::<Vec<_>>();
        let upstream = self
            .upstream(&self.scope.chapter_id, "page_prompt", Some(i64::MAX))
            .into_iter()
            .filter(|r| r.chapter != self.scope.chapter_id)
            .map(|r| {
                (
                    &r.document.id,
                    if r.document.kind == "page_prompt" {
                        r.state_hash.clone()
                    } else if r.document.kind == "script" {
                        r.supplement_hash.clone()
                    } else {
                        r.document.content_hash.clone()
                    },
                    r.document.stale,
                    r.document.out_of_plan,
                    &r.document.issues,
                )
            })
            .collect::<Vec<_>>();
        hash(
            &serde_json::to_string(&(
                current.map(|c| (&c.id, c.sequence, &c.source_id)),
                local,
                upstream,
            ))
            .unwrap(),
        )
    }
    pub(super) fn context(&self, kind: &str, page: Option<i64>) -> Result<String, String> {
        let mut context = format!(
            "# 最新关联资料\n## 当前章节正文\n{}",
            self.chapters
                .iter()
                .find(|ch| ch.id == self.scope.chapter_id)
                .map(|ch| ch.content.as_str())
                .unwrap_or_default()
        );
        for r in self.upstream(&self.scope.chapter_id, kind, page) {
            let d = &r.document;
            if d.stale || !d.issues.is_empty() {
                return Err(format!(
                    "{}尚未更新或缺少必要节点，请先更新上游资料；你的当前文档仍可手工核对保存",
                    document_label(d)
                ));
            }
            let (title, md) = if d.kind == "page_prompt" {
                (
                    format!(
                        "前页人物状态（第{}章第{}页）",
                        self.chapters
                            .iter()
                            .find(|ch| ch.id == r.chapter)
                            .map_or(0, |ch| ch.number),
                        d.page_no.unwrap_or(0)
                    ),
                    r.state_text.clone(),
                )
            } else if r.chapter != self.scope.chapter_id && d.kind == "script" {
                ("前章人物状态补充".into(), r.supplement.clone())
            } else {
                (
                    match d.kind.as_str() {
                        "settings" => "最新作品设定",
                        "script" => "最新本章剧本",
                        _ => "最新分页分镜",
                    }
                    .into(),
                    d.markdown.clone(),
                )
            };
            context.push_str(&format!("\n\n## {title}\n{md}"));
        }
        Ok(context)
    }
    pub(super) fn plan(&self) -> SyncPlan {
        let docs = self.documents();
        let targets = docs
            .iter()
            .filter(|d| d.stale && !d.out_of_plan)
            .map(|d| SyncTarget {
                document_id: d.id.clone(),
                revision: d.revision,
                kind: d.kind.clone(),
                page_no: d.page_no,
                reasons: d.stale_reasons.clone(),
            })
            .collect::<Vec<_>>();
        let page_plan = self.page_plan(&self.scope.chapter_id).unwrap_or_default();
        let missing_page_nos = page_plan
            .into_iter()
            .filter(|p| {
                !docs
                    .iter()
                    .any(|d| d.kind == "page_prompt" && d.page_no == Some(*p))
            })
            .collect::<Vec<_>>();
        let obsolete_page_nos = docs
            .iter()
            .filter(|d| d.out_of_plan)
            .filter_map(|d| d.page_no)
            .collect::<Vec<_>>();
        let mut blocked_reason = None;
        for t in &targets {
            for upstream in self.upstream(&self.scope.chapter_id, &t.kind, t.page_no) {
                if upstream.document.stale
                    && upstream.chapter != self.scope.chapter_id
                    && upstream.document.kind != "settings"
                {
                    blocked_reason =
                        Some("前章关联资料尚未更新，请先到前章同步或核对，再更新本章".into());
                } else if !upstream.document.issues.is_empty()
                    && !targets
                        .iter()
                        .any(|t| t.document_id == upstream.document.id)
                {
                    blocked_reason = Some(format!(
                        "请先补全{}的必要节点",
                        document_label(&upstream.document)
                    ));
                }
            }
        }
        let fingerprint = hash(
            &serde_json::to_string(&(
                self.guard(),
                &targets,
                &missing_page_nos,
                &obsolete_page_nos,
            ))
            .unwrap(),
        );
        SyncPlan {
            fingerprint,
            targets,
            missing_page_nos,
            obsolete_page_nos,
            blocked_reason,
        }
    }
    pub(super) fn affected(&self) -> Vec<AffectedChapter> {
        self.chapters
            .iter()
            .filter(|ch| ch.id != self.scope.chapter_id)
            .filter_map(|ch| {
                let count = self
                    .raw
                    .iter()
                    .filter(|r| r.chapter == ch.id && r.document.stale && !r.document.out_of_plan)
                    .count();
                (count > 0).then(|| AffectedChapter {
                    chapter_id: ch.id.clone(),
                    chapter_no: ch.number,
                    title: ch.title.clone(),
                    document_count: count,
                    reason: "关联资料或前页人物状态变化，后续内容可能受影响，请同步或核对".into(),
                })
            })
            .collect()
    }
}
