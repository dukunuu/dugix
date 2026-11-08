use super::*;
use test_support::{Temp, sample};
#[test]
fn png_pages_and_optional_visual_previews() {
    let temp = Temp::new();
    temp.image();
    let mut doc = sample();
    doc.theme.colors.insert("bg".into(), "#ffffff".into());
    doc.theme.colors.insert("fg".into(), "#1d1d1f".into());
    doc.theme.colors.insert("surface".into(), "#f5f5f7".into());
    doc.theme.colors.insert("border".into(), "#d2d2d7".into());
    let pages = layout::paginate(&doc, Some(&temp.0.join("assets")));
    let preview = std::env::var_os("NEXUS_DOC_PREVIEWS").is_some();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/doc-previews");
    if preview {
        std::fs::create_dir_all(&directory).unwrap();
    }
    for (index, page) in pages.iter().enumerate() {
        let bytes = render::png(&doc, page, Some(&temp.0.join("assets")), 700).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!(image.width(), 700);
        if preview {
            std::fs::write(directory.join(format!("page-{}.png", index + 1)), bytes).unwrap();
        }
    }
}
#[test]
fn long_document_previews_when_requested() {
    let Some(_) = std::env::var_os("NEXUS_DOC_PREVIEWS") else {
        return;
    };
    let doc = test_support::long();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/doc-previews");
    std::fs::create_dir_all(&directory).unwrap();
    for (index, page) in layout::paginate(&doc, None).iter().enumerate() {
        std::fs::write(
            directory.join(format!("long-{}.png", index + 1)),
            render::png(&doc, page, None, 700).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn html_is_semantic_self_contained_and_printable() {
    let temp = Temp::new();
    temp.image();
    let doc = sample();
    let html = export::html::export(&doc, Some(&temp.0.join("assets")), &mut vec![]).unwrap();
    assert!(html.contains("Content-Security-Policy") && html.contains("default-src 'none'"));
    assert!(html.contains("@page{size:595.28pt 841.89pt;margin:72pt 72pt 72pt 72pt}"));
    assert!(html.contains("@media print") && html.contains("break-before:page"));
    for tag in [
        "<h1",
        "<h2",
        "<p ",
        "<ol>",
        "<blockquote",
        "<aside",
        "<pre",
        "<table",
        "<figure",
        "<figcaption",
        "<hr",
        "<nav",
    ] {
        assert!(html.contains(tag), "{tag}");
    }
    assert!(html.contains("data:image/png;base64,") && html.contains("<s>strike</s>"));
    assert!(
        !html.contains("src=\"https:")
            && !html.contains("src=\"http:")
            && !html.contains("@import")
            && !html.contains("<script")
    );
    assert_eq!(
        html,
        export::html::export(&doc, Some(&temp.0.join("assets")), &mut vec![]).unwrap()
    );
}
#[test]
fn suggestion_rechecks_keep_custom_design_resolver_through_storage() {
    let temp = Temp::new();
    let doc = sample();
    let mut design = crate::design::Design::default();
    design.name = "Custom".into();
    design.colors.insert("accent".into(), "#123456".into());
    crate::design::save(&temp.0, "custom", &design).unwrap();
    let resolver = |slug: &str| crate::design::load(&temp.0, slug);
    let (mut valid, errors) = suggest::prepare_with_resolver(
        &doc,
        vec![suggest::RawSuggestion {
            why: "Use the shared design".into(),
            ops: vec![serde_json::json!({"op":"apply_design","design":"custom"})],
        }],
        Some(&resolver),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let mut thread = test_support::thread(&doc, false);
    thread.messages[0].edits.push(feedback::Edit::new(
        &doc,
        valid.pop().unwrap(),
        &thread.anchor,
    ));
    let path = temp.0.join("docs/report.doc.json");
    feedback::save(&path, &[thread]).unwrap();
    storage::save(&temp.0, "docs/report.doc.json", &doc, history::Who::Model).unwrap();
    let mut threads = feedback::load(&path).unwrap();
    assert_eq!(
        threads[0].messages[0].edits[0].status,
        feedback::EditStatus::Pending
    );
    let mut changed = doc.clone();
    changed.title = "An unrelated edit".into();
    feedback::reanchor_with_resolver(&mut threads, &changed, Some(&resolver));
    assert_eq!(
        threads[0].messages[0].edits[0].status,
        feedback::EditStatus::Pending
    );
    let mut stale = doc.clone();
    stale.theme.colors.insert("accent".into(), "#654321".into());
    feedback::reanchor_with_resolver(&mut threads, &stale, Some(&resolver));
    assert_eq!(
        threads[0].messages[0].edits[0].status,
        feedback::EditStatus::Outdated
    );
}
#[test]
fn html_and_docx_color_code_by_syntax() {
    let doc = crate::doc::test_support::checked(serde_json::json!({
        "version": 1,
        "title": "Code",
        "blocks": [{"id": "c", "kind": "code", "lang": "rust", "text": "let x = \"<y>\";"}]
    }));
    let html = export::html::export(&doc, None, &mut vec![]).unwrap();
    assert!(
        html.contains("<span class=\"hl-accent\">let</span>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"hl-accent2\">&quot;&lt;y&gt;&quot;</span>")
            || html.contains("<span class=\"hl-accent2\">\"&lt;y&gt;\"</span>"),
        "{html}"
    );
    assert!(html.contains(".hl-accent{color:"));
    let bytes = export::docx::export(&doc, None, &[], &mut vec![]).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml)
        .unwrap();
    let theme = crate::doc::inline::theme(&doc);
    let accent = crate::deck::text::color(&theme, "accent");
    assert!(
        xml.contains(&format!(
            "<w:color w:val=\"{}\"/>",
            accent.trim_start_matches('#')
        )),
        "{xml}"
    );
}
