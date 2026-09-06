fn page_put(c: &Connection, s: &Scope, n: i64, md: &str, revision: Option<i64>) -> Document {
    save(
        c,
        &SaveInput {
            scope: s.clone(),
            kind: "page_prompt".into(),
            page_no: Some(n),
            markdown: md.replace("第1页", &format!("第{n}页")),
            optimization_instruction: String::new(),
            expected_revision: revision,
            acknowledge_updates: true,
        },
    )
    .unwrap()
}
fn metadata_put(c: &Connection, s: &Scope, d: &Document) -> Document {
    save(
        c,
        &SaveInput {
            scope: s.clone(),
            kind: d.kind.clone(),
            page_no: d.page_no,
            markdown: d.markdown.clone(),
            optimization_instruction: "只改优化要求".into(),
            expected_revision: Some(d.revision),
            acknowledge_updates: false,
        },
    )
    .unwrap()
}
#[test]
fn metadata_does_not_stale_content_or_images_and_stale_cannot_be_washed() {
    let (c, s) = setup();
    pipeline(&c, &s);
    let page = page_put(&c, &s, 1, PROMPT, None);
    let j = insert_job(&c, &s, "render", "{}", 1).unwrap();
    let image_path = std::env::temp_dir().join(format!("comic-md-image-{}.png", id()));
    std::fs::write(&image_path, b"image").unwrap();
    c.execute("INSERT INTO comic_md_images(id,job_id,document_id,document_revision,page_no,path,created_at) VALUES('hash-image',?,?,?,1,?,0)",params![j.id,page.id,page.revision,image_path.display().to_string()]).unwrap();
    let next = metadata_put(&c, &s, &page);
    assert_eq!(page.content_hash, next.content_hash);
    assert_eq!(next.revision, 2);
    assert!(!workspace(&c, &s).unwrap().images[0].stale);
    std::fs::remove_file(&image_path).unwrap();
    assert!(workspace(&c, &s).unwrap().images[0].stale);
    let setting = documents(&c, &s)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "settings")
        .unwrap();
    metadata_put(&c, &s, &setting);
    assert!(!documents(&c, &s).unwrap().iter().any(|d| d.stale));
    put(
        &c,
        &s,
        "settings",
        &SETTINGS.replace("黑发", "银发"),
        Some(2),
    );
    let stale = documents(&c, &s)
        .unwrap()
        .into_iter()
        .find(|d| d.id == page.id)
        .unwrap();
    assert!(stale.stale);
    let metadata = metadata_put(&c, &s, &stale);
    assert!(metadata.stale);
    let edited = page_put(
        &c,
        &s,
        1,
        &PROMPT.replace("远景", "近景"),
        Some(metadata.revision),
    );
    assert!(
        edited.stale,
        "stale script/storyboard must propagate through explicit save"
    );
}
#[test]
fn manual_body_edit_keeps_stale_dependencies_until_explicit_acknowledgement() {
    let (c, s) = setup();
    let first = page_put(&c, &s, 1, PROMPT, None);
    let second = page_put(&c, &s, 2, PROMPT, None);
    page_put(
        &c,
        &s,
        1,
        &first.markdown.replace("左臂包扎", "右腿受伤"),
        Some(first.revision),
    );
    assert!(
        documents(&c, &s)
            .unwrap()
            .iter()
            .find(|document| document.id == second.id)
            .unwrap()
            .stale
    );
    let edited = save(
        &c,
        &SaveInput {
            scope: s.clone(),
            kind: "page_prompt".into(),
            page_no: Some(2),
            markdown: second.markdown.replace("远景", "近景"),
            optimization_instruction: "手工核对中".into(),
            expected_revision: Some(second.revision),
            acknowledge_updates: false,
        },
    )
    .unwrap();
    assert!(edited.stale, "普通保存不能暗中确认人物状态依赖");
    let acknowledged = save(
        &c,
        &SaveInput {
            scope: s,
            kind: "page_prompt".into(),
            page_no: Some(2),
            markdown: edited.markdown,
            optimization_instruction: edited.optimization_instruction,
            expected_revision: Some(edited.revision),
            acknowledge_updates: true,
        },
    )
    .unwrap();
    assert!(!acknowledged.stale);
}
#[test]
fn local_frames_do_not_affect_later_pages_but_state_affects_following_chapter_script() {
    let (c, s) = setup();
    let first = page_put(&c, &s, 1, PROMPT, None);
    let second = page_put(&c, &s, 2, PROMPT, None);
    let later = Scope {
        chapter_id: "ch2".into(),
        ..s.clone()
    };
    put(&c, &later, "script", SCRIPT, None);
    let first = page_put(
        &c,
        &s,
        1,
        &PROMPT.replace("远景", "特写"),
        Some(first.revision),
    );
    assert!(
        !documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == second.id)
            .unwrap()
            .stale
    );
    assert!(!documents(&c, &later).unwrap()[0].stale);
    page_put(
        &c,
        &s,
        1,
        &first.markdown.replace("左臂包扎", "右腿受伤"),
        Some(first.revision),
    );
    assert!(
        documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == second.id)
            .unwrap()
            .stale
    );
    assert!(documents(&c, &later).unwrap()[0].stale);
    assert_eq!(workspace(&c, &s).unwrap().affected_chapters.len(), 1);
}
#[test]
fn legacy_receipts_resolve_original_revision_and_metadata_preserves_old_state_baseline() {
    let (c, s) = setup();
    let first = page_put(&c, &s, 1, PROMPT, None);
    c.execute(
        "UPDATE comic_md_revisions SET created_at=100 WHERE document_id=?",
        [&first.id],
    )
    .unwrap();
    let second = page_put(&c, &s, 2, PROMPT, None);
    let legacy = r#"[["source","src1"],["settings",null],["script",null],["storyboard",null]]"#;
    c.execute(
        "UPDATE comic_md_documents SET dependencies=? WHERE id=?",
        params![legacy, second.id],
    )
    .unwrap();
    c.execute(
        "UPDATE comic_md_revisions SET dependencies=?,created_at=200 WHERE document_id=?",
        params![legacy, second.id],
    )
    .unwrap();
    assert!(
        !documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == second.id)
            .unwrap()
            .stale
    );
    page_put(&c, &s, 1, &PROMPT.replace("左臂包扎", "右腿受伤"), Some(1));
    c.execute(
        "UPDATE comic_md_revisions SET created_at=300 WHERE document_id=? AND revision=2",
        [&first.id],
    )
    .unwrap();
    let stale = documents(&c, &s)
        .unwrap()
        .into_iter()
        .find(|d| d.id == second.id)
        .unwrap();
    assert!(stale.stale);
    assert!(metadata_put(&c, &s, &stale).stale);
    c.execute(
        "UPDATE comic_md_revisions SET created_at=100 WHERE document_id=? AND revision=2",
        [&first.id],
    )
    .unwrap();
    assert!(documents(&c, &s)
        .unwrap()
        .iter()
        .find(|d| d.id == second.id)
        .unwrap()
        .stale_reasons
        .iter()
        .any(|r| r.contains("不可核验")));
}
#[test]
fn legacy_referenced_revision_hash_survives_metadata_and_missing_history_is_conservative() {
    let (c, s) = setup();
    let setting = put(&c, &s, "settings", SETTINGS, None);
    let script = put(&c, &s, "script", SCRIPT, None);
    let legacy = json!([["source", "src1"], ["settings", [setting.id, 1]]]).to_string();
    c.execute(
        "UPDATE comic_md_documents SET dependencies=? WHERE id=?",
        params![legacy, script.id],
    )
    .unwrap();
    c.execute(
        "UPDATE comic_md_revisions SET dependencies=? WHERE document_id=?",
        params![legacy, script.id],
    )
    .unwrap();
    metadata_put(&c, &s, &setting);
    assert!(
        !documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == script.id)
            .unwrap()
            .stale
    );
    c.execute(
        "DELETE FROM comic_md_revisions WHERE document_id=? AND revision=1",
        [&setting.id],
    )
    .unwrap();
    assert!(
        documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == script.id)
            .unwrap()
            .stale
    );
}
#[test]
fn batch_refreshes_state_context_after_own_write_and_rejects_external_edits() {
    let (c, s) = setup();
    let first = page_put(&c, &s, 1, PROMPT, None);
    let second = page_put(&c, &s, 2, PROMPT, None);
    let mut f = freeze_optimization(
        &c,
        &OptimizeInput {
            scope: s.clone(),
            targets: vec![
                PageInput {
                    document_id: first.id.clone(),
                    revision: 1,
                },
                PageInput {
                    document_id: second.id.clone(),
                    revision: 1,
                },
            ],
            instruction: "右腿受伤".into(),
            all_pages: true,
        },
    )
    .unwrap();
    let j = insert_job(&c, &s, "optimize", "{}", 2).unwrap();
    apply_optimization(
        &c,
        &j.id,
        &f,
        &f.targets[0],
        completed(&PROMPT.replace("左臂包扎", "右腿受伤")),
    )
    .unwrap();
    f.guard = lineage::Book::load(&c, &s).unwrap().guard();
    let target = refresh_optimization(&c, &f, &f.targets[1]).unwrap();
    assert!(target.prompt.contains("前页人物状态（第1章第1页）"));
    assert!(target.prompt.contains("右腿受伤"));
    apply_optimization(
        &c,
        &j.id,
        &f,
        &target,
        completed(
            &PROMPT
                .replace("第1页", "第2页")
                .replace("左臂包扎", "右腿受伤"),
        ),
    )
    .unwrap();
    assert!(!documents(&c, &s).unwrap().iter().any(|d| d.stale));
    f.guard = lineage::Book::load(&c, &s).unwrap().guard();
    let first = documents(&c, &s).unwrap()[0].clone();
    metadata_put(&c, &s, &first);
    assert!(refresh_optimization(&c, &f, &f.targets[1])
        .err()
        .unwrap()
        .contains("外部编辑"));
}
#[test]
fn sync_orders_missing_pages_before_existing_later_pages_and_retains_partial_failure() {
    let (c, s) = setup();
    let board = format!(
        "{BOARD}\n{}\n{}",
        BOARD.replace("第1页", "第2页"),
        BOARD.replace("第1页", "第3页")
    );
    put(&c, &s, "storyboard", &board, None);
    page_put(&c, &s, 1, PROMPT, None);
    let third = page_put(&c, &s, 3, PROMPT, None);
    let b = lineage::Book::load(&c, &s).unwrap();
    assert_eq!(b.plan().missing_page_nos, vec![2]);
    let guard = b.guard();
    let allowed = [third.id.clone()].into_iter().collect();
    let missing = [2].into_iter().collect();
    let step = sync::prepare(&c, &s, &guard, &allowed, &missing)
        .unwrap()
        .unwrap();
    assert_eq!(step.page, Some(2));
    let j = insert_job(&c, &s, "sync", "{}", 1).unwrap();
    let guard = sync::apply(
        &c,
        &s,
        &j.id,
        &step,
        completed(
            &PROMPT
                .replace("第1页", "第2页")
                .replace("左臂包扎", "右腿受伤"),
        ),
    )
    .unwrap();
    let step = sync::prepare(&c, &s, &guard, &allowed, &missing)
        .unwrap()
        .unwrap();
    assert_eq!(step.page, Some(3));
    assert!(step.prompt.contains("右腿受伤"));
    assert!(sync::apply(&c, &s, &j.id, &step, completed("# 第3页\n半成品")).is_err());
    assert_eq!(
        documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == third.id)
            .unwrap()
            .revision,
        1
    );
    assert_eq!(jobs(&c, &s).unwrap()[0].completed_pages, 1);
    assert!(jobs(&c, &s).unwrap()[0]
        .output_markdown
        .as_ref()
        .unwrap()
        .contains("半成品"));
}
#[test]
fn page_plan_contraction_keeps_history_and_default_export_omits_obsolete_pages() {
    let (c, s) = setup();
    put(
        &c,
        &s,
        "storyboard",
        &format!("{BOARD}\n{}", BOARD.replace("第1页", "第2页")),
        None,
    );
    page_put(&c, &s, 1, PROMPT, None);
    let second = page_put(&c, &s, 2, PROMPT, None);
    put(&c, &s, "storyboard", BOARD, Some(1));
    let book = lineage::Book::load(&c, &s).unwrap();
    assert_eq!(book.plan().obsolete_page_nos, vec![2]);
    assert!(
        book.documents()
            .iter()
            .find(|d| d.id == second.id)
            .unwrap()
            .out_of_plan
    );
    assert!(render_pages(
        &c,
        &RenderInput {
            scope: s.clone(),
            pages: vec![PageInput {
                document_id: second.id.clone(),
                revision: 1
            }],
            expected_render_options_revision: None,
            rerun_prompt_injection: String::new()
        }
    )
    .is_err());
    let dir = std::env::temp_dir().join(format!("comic-md-lineage-{}", id()));
    let result = export_to(
        &c,
        &ExportInput {
            scope: s.clone(),
            document_ids: None,
        },
        &dir,
    )
    .unwrap();
    assert_eq!(result.files.len(), 2);
    let explicit = export_to(
        &c,
        &ExportInput {
            scope: s,
            document_ids: Some(vec![second.id]),
        },
        &dir,
    )
    .unwrap();
    assert_eq!(explicit.files.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn guard_ignores_future_and_upstream_metadata_but_detects_current_target_edits() {
    let (c, s) = setup();
    let setting = put(&c, &s, "settings", SETTINGS, None);
    let page = page_put(&c, &s, 1, PROMPT, None);
    let original = lineage::Book::load(&c, &s).unwrap().guard();
    metadata_put(&c, &s, &setting);
    assert_eq!(original, lineage::Book::load(&c, &s).unwrap().guard());
    let future = Scope {
        chapter_id: "ch3".into(),
        ..s.clone()
    };
    page_put(&c, &future, 1, PROMPT, None);
    assert_eq!(original, lineage::Book::load(&c, &s).unwrap().guard());
    metadata_put(&c, &s, &page);
    assert_ne!(original, lineage::Book::load(&c, &s).unwrap().guard());
}
#[test]
fn previous_dialogue_does_not_stale_following_script_but_previous_state_does() {
    let (c, s) = setup();
    let first = put(&c, &s, "script", SCRIPT, None);
    let later = Scope {
        chapter_id: "ch2".into(),
        ..s.clone()
    };
    put(&c, &later, "script", SCRIPT, None);
    put(
        &c,
        &s,
        "script",
        &SCRIPT.replace("别出声", "快进来"),
        Some(first.revision),
    );
    assert!(!documents(&c, &later).unwrap()[0].stale);
    put(
        &c,
        &s,
        "script",
        &SCRIPT.replace("左臂包扎", "右腿受伤"),
        Some(2),
    );
    assert!(documents(&c, &later).unwrap()[0].stale);
}
#[test]
fn sync_updates_upstream_first_preserves_old_instruction_and_checks_late_cas() {
    let (c, s) = setup();
    pipeline(&c, &s);
    page_put(&c, &s, 1, PROMPT, None);
    let script = documents(&c, &s)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "script")
        .unwrap();
    metadata_put(&c, &s, &script);
    put(
        &c,
        &s,
        "settings",
        &SETTINGS.replace("黑发", "银发"),
        Some(1),
    );
    let book = lineage::Book::load(&c, &s).unwrap();
    let plan = book.plan();
    assert_eq!(
        plan.targets
            .iter()
            .map(|t| t.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["script", "storyboard", "page_prompt"]
    );
    let allowed = plan.targets.iter().map(|t| t.document_id.clone()).collect();
    let missing = std::collections::BTreeSet::new();
    let mut guard = book.guard();
    let j = insert_job(&c, &s, "sync", "{}", 3).unwrap();
    for (kind, md) in [("script", SCRIPT), ("storyboard", BOARD)] {
        let step = sync::prepare(&c, &s, &guard, &allowed, &missing)
            .unwrap()
            .unwrap();
        assert_eq!(step.kind, kind);
        assert!(step.prompt.contains("最新作品设定"));
        assert!(step.prompt.contains("银发"));
        assert!(!step.prompt.contains("只改优化要求"));
        if kind == "storyboard" {
            let expanded = format!("{BOARD}\n\n{}", BOARD.replace("第1页", "第2页"));
            let error = sync::apply(&c, &s, &j.id, &step, completed(&expanded)).unwrap_err();
            assert!(error.contains("不会自动改变分页数量"));
            assert_eq!(
                documents(&c, &s)
                    .unwrap()
                    .iter()
                    .find(|document| document.kind == "storyboard")
                    .unwrap()
                    .revision,
                1
            );
        }
        guard = sync::apply(&c, &s, &j.id, &step, completed(md)).unwrap();
    }
    let current = documents(&c, &s).unwrap();
    assert_eq!(
        current
            .iter()
            .find(|d| d.kind == "script")
            .unwrap()
            .optimization_instruction,
        "只改优化要求"
    );
    let step = sync::prepare(&c, &s, &guard, &allowed, &missing)
        .unwrap()
        .unwrap();
    let page = current.iter().find(|d| d.kind == "page_prompt").unwrap();
    metadata_put(&c, &s, page);
    assert!(sync::apply(&c, &s, &j.id, &step, completed(PROMPT))
        .unwrap_err()
        .contains("外部编辑"));
    assert_eq!(jobs(&c, &s).unwrap()[0].completed_pages, 2);
}
#[test]
fn legacy_removed_page_state_does_not_disappear_from_the_historical_receipt() {
    let (c, s) = setup();
    let board = put(
        &c,
        &s,
        "storyboard",
        &format!("{BOARD}\n{}", BOARD.replace("第1页", "第2页")),
        None,
    );
    let second = page_put(&c, &s, 2, PROMPT, None);
    c.execute("UPDATE comic_md_revisions SET created_at=100", [])
        .unwrap();
    let later = Scope {
        chapter_id: "ch2".into(),
        ..s.clone()
    };
    let script = put(&c, &later, "script", SCRIPT, None);
    let legacy = r#"[["source","src2"],["settings",null],["ch1","src1",null,null]]"#;
    c.execute(
        "UPDATE comic_md_documents SET dependencies=? WHERE id=?",
        params![legacy, script.id],
    )
    .unwrap();
    c.execute(
        "UPDATE comic_md_revisions SET dependencies=?,created_at=200 WHERE document_id=?",
        params![legacy, script.id],
    )
    .unwrap();
    assert!(
        !documents(&c, &later)
            .unwrap()
            .iter()
            .find(|d| d.id == script.id)
            .unwrap()
            .stale
    );
    put(&c, &s, "storyboard", BOARD, Some(board.revision));
    c.execute(
        "UPDATE comic_md_revisions SET created_at=300 WHERE document_id=? AND revision=2",
        [&board.id],
    )
    .unwrap();
    assert!(
        documents(&c, &s)
            .unwrap()
            .iter()
            .find(|d| d.id == second.id)
            .unwrap()
            .out_of_plan
    );
    assert!(
        documents(&c, &later)
            .unwrap()
            .iter()
            .find(|d| d.id == script.id)
            .unwrap()
            .stale
    );
}
