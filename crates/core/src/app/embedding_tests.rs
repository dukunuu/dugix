//! Local EmbeddingGemma 2 flows against an in-process mock Ollama.

use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::app::tests::app_with_key;
use crate::provider::embedding::tests::MockOllama;
use crate::provider::local::LocalConfig;

fn point_at(app: &mut App, endpoint: &str) {
    app.saved.local = Some(LocalConfig {
        provider: LocalRuntime::Ollama,
        endpoint: Some(endpoint.to_string()),
        list_command: None,
        memory_budget_mb: None,
        context_length: None,
        kv_cache_q8: false,
        parallel_tool_calls: None,
        tier: None,
    });
}

async fn dead_endpoint() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    endpoint
}

async fn drain(app: &mut App) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..50 {
        let Some(rx) = app.index.embed_rx.as_mut() else {
            return seen;
        };
        let message = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("embedding job stalled");
        app.on_embed_done(message);
        seen.push(app.index.embedding_status.clone());
    }
    panic!("embedding pipeline did not settle");
}

fn space_with_files(app: &App) -> std::path::PathBuf {
    let dir = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "quarterly financial report").unwrap();
    image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 7]))
        .save(dir.join("beach.png"))
        .unwrap();
    dir
}

fn stored(app: &App) -> Vec<(String, String, String)> {
    let mut stmt = app
        .db
        .raw()
        .prepare(
            "SELECT files.name, e.location, e.identity FROM cache.chunk_embeddings e
             JOIN files ON files.id = e.file_id ORDER BY files.name, e.seq",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn documents(mock: &MockOllama) -> Vec<String> {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter_map(|input| input.as_str().map(str::to_owned))
        .filter(|text| text.starts_with("title:"))
        .collect()
}

fn image_inputs(mock: &MockOllama) -> usize {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(path, _)| path == "/api/embed")
        .flat_map(|(_, body)| body["input"].as_array().cloned().unwrap_or_default())
        .filter(|input| input.get("image").is_some())
        .count()
}

#[tokio::test]
async fn local_model_embeds_text_and_images_directly_and_queues_no_convert() {
    let mock = MockOllama::start(true).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.index.embedding_model = "local:embeddinggemma-2".to_string();

    app.rescan_files();
    let seen = drain(&mut app).await;
    assert!(seen.iter().any(|s| s.contains("indexing")), "{seen:?}");

    assert!(
        app.index.convert_rx.is_none(),
        "no OCR or caption job for the image"
    );
    let beach = app
        .caches
        .files
        .iter()
        .find(|f| f.name == "beach.png")
        .unwrap();
    assert_eq!(beach.status, "ok · image");
    assert_eq!(
        documents(&mock),
        vec!["title: notes.txt | text: quarterly financial report"]
    );
    assert_eq!(
        image_inputs(&mock),
        2,
        "one capability probe plus the real image"
    );
    let rows = stored(&app);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "beach.png");
    assert_eq!(rows[0].1, "image");
    assert_eq!(rows[1].1, "lines 1-1");
    let identity: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(identity["model"], "embeddinggemma-2");
    assert_eq!(identity["dimension"], 768);
    assert_eq!(identity["revision"], "sha256:one");
    assert_eq!(rows[0].2, rows[1].2);
    assert_eq!(
        app.index.embedding_status,
        "embeddings: local · embeddinggemma-2 · 768d · images"
    );
}

fn scanned_pdf(page_count: u32) -> Vec<u8> {
    use pdf_extract::{Document, Object};

    let mut pdf = Document::load_mem(&crate::extract::minimal_pdf(None)).unwrap();
    let page = pdf.get_object((3, 0)).unwrap().clone();
    let mut kids = vec![Object::Reference((3, 0))];
    for _ in 1..page_count {
        kids.push(Object::Reference(pdf.add_object(page.clone())));
    }
    let pages = pdf.get_object_mut((2, 0)).unwrap().as_dict_mut().unwrap();
    pages.set("Kids", kids);
    pages.set("Count", i64::from(page_count));
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn scanned_pdf_embedding_uses_pages_only_with_image_support() {
    let can_render = ["pdfinfo", "pdftoppm"]
        .iter()
        .all(|tool| std::process::Command::new(tool).arg("-v").output().is_ok());
    let pdf = scanned_pdf(2);
    for images in [true, false] {
        if images && !can_render {
            eprintln!("skipping scanned_pdf_embedding images: pdfinfo or pdftoppm not installed");
            continue;
        }
        let mock = MockOllama::start(images).await;
        let mut app = app_with_key();
        point_at(&mut app, &mock.endpoint);
        let dir = app.space.files_dir(&app.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scan.PDF"), &pdf).unwrap();
        app.index.embedding_model = "local:embeddinggemma-2".to_string();

        app.rescan_files();
        drain(&mut app).await;

        assert!(app.index.convert_rx.is_none());
        assert_eq!(
            app.caches.files[0].status,
            "ok · scanned pdf (no text layer)"
        );
        let rows = stored(&app);
        if images {
            assert_eq!(
                rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
                ["page 1", "page 2"]
            );
            assert_eq!(image_inputs(&mock), 3, "one probe plus two pages");
            assert!(documents(&mock).is_empty());
            for page in 1..=2 {
                let image = image::open(dir.join(format!("scan/page-{page}.png"))).unwrap();
                assert_eq!((image.width(), image.height()), (834, 313));
            }
            assert!(!dir.join("scan/page-3.png").exists());
        } else {
            assert_eq!(rows.len(), 1);
            assert_eq!(image_inputs(&mock), 0);
            let docs = documents(&mock);
            assert_eq!(docs.len(), 1);
            assert!(docs[0].contains("uploaded file: scan.PDF"), "{docs:?}");
            assert!(!dir.join("scan").exists());
        }
        app.rescan_files();
        drain(&mut app).await;
        assert_eq!(stored(&app), rows, "the same PDF is not re-embedded");
        assert_eq!(image_inputs(&mock), if images { 4 } else { 0 });
    }
}

#[tokio::test]
async fn scanned_pdf_embedding_reuses_cached_pages_and_stops_at_thirty() {
    if std::process::Command::new("pdfinfo")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("skipping scanned_pdf_embedding cached pages: pdfinfo not installed");
        return;
    }
    let mock = MockOllama::start(true).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    let dir = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(dir.join("scan")).unwrap();
    std::fs::write(dir.join("scan.pdf"), scanned_pdf(31)).unwrap();
    for page in 1..=31 {
        image::RgbImage::new(2, 2)
            .save(dir.join(format!("scan/page-{page}.png")))
            .unwrap();
    }
    app.index.embedding_model.clear();
    app.rescan_files();
    app.db
        .set_file_status(
            &app.caches.files[0].id,
            "embedding… · ok · scanned pdf (no text layer)",
        )
        .unwrap();
    app.index.embedding_model = "local:embeddinggemma-2".to_string();
    app.start_embedding();
    drain(&mut app).await;

    let rows = stored(&app);
    assert_eq!(rows.len(), 30);
    assert_eq!(rows[0].1, "page 1");
    assert_eq!(rows[29].1, "page 30");
    assert_eq!(image_inputs(&mock), 31, "one probe plus thirty pages");
    assert!(documents(&mock).is_empty());
    assert_eq!(
        app.caches.files[0].status,
        "ok · scanned pdf (no text layer)"
    );
    let cached = image::open(dir.join("scan/page-1.png")).unwrap();
    assert_eq!((cached.width(), cached.height()), (2, 2));
}

#[tokio::test]
async fn same_identity_is_not_reembedded_but_a_new_digest_is() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.index.embedding_model = "local:embeddinggemma-2:270m".to_string();

    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 2);
    let first = stored(&app)[0].2.clone();

    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 2);
    assert!(stored(&app).iter().all(|row| row.2 == first));

    mock.state.lock().unwrap().digest = "sha256:two".to_string();
    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(documents(&mock).len(), 4);
    let rows = stored(&app);
    assert!(!rows.is_empty());
    assert!(
        rows.iter()
            .all(|row| row.2 != first && row.2.contains("sha256:two")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn discovering_the_local_model_replaces_old_vectors_lazily() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    space_with_files(&app);
    app.index.embedding_model.clear();
    app.rescan_files();
    let id = app
        .caches
        .files
        .iter()
        .find(|f| f.name == "notes.txt")
        .unwrap()
        .id
        .clone();
    app.db
        .set_file_embeddings(
            &id,
            "old-identity",
            &[("lines 1-1".into(), "x".into())],
            &[(0, vec![1.0, 0.0])],
        )
        .unwrap();

    app.discover_embeddings();
    assert!(app.index.embed_rx.is_some());
    drain(&mut app).await;
    assert_eq!(app.index.embedding_model, "local:embeddinggemma-2:270m");
    let rows = stored(&app);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.2 != "old-identity"), "{rows:?}");
}

#[tokio::test]
async fn default_is_local_only_when_reachable() {
    let mock = MockOllama::start(false).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    assert_eq!(app.index.embedding_model, "google/gemini-embedding-2");
    app.discover_embeddings();
    drain(&mut app).await;
    assert_eq!(app.index.embedding_model, "local:embeddinggemma-2:270m");
    assert!(
        app.db
            .load_settings()
            .unwrap()
            .iter()
            .all(|(k, _)| k != "embedding_model")
    );

    for stored_choice in ["", "google/gemini-embedding-2"] {
        let mut stale = app_with_key();
        point_at(&mut stale, &mock.endpoint);
        stale
            .db
            .set_setting("embedding_model", stored_choice)
            .unwrap();
        stale.discover_embeddings();
        assert!(
            stale.index.embed_rx.is_some(),
            "a stored {stored_choice:?} must not block discovery"
        );
        drain(&mut stale).await;
        assert_eq!(stale.index.embedding_model, "local:embeddinggemma-2:270m");
    }

    let mut unreachable = app_with_key();
    let dead = dead_endpoint().await;
    point_at(&mut unreachable, &dead);
    unreachable.discover_embeddings();
    drain(&mut unreachable).await;
    assert_eq!(
        unreachable.index.embedding_model,
        "google/gemini-embedding-2"
    );

    mock.state.lock().unwrap().installed = false;
    let mut missing = app_with_key();
    point_at(&mut missing, &mock.endpoint);
    missing.discover_embeddings();
    drain(&mut missing).await;
    assert_eq!(missing.index.embedding_model, "google/gemini-embedding-2");
}

#[tokio::test]
async fn choosing_local_never_falls_back_to_a_cloud_embedder() {
    let mut app = app_with_key();
    let dead = dead_endpoint().await;
    point_at(&mut app, &dead);
    space_with_files(&app);
    app.index.embedding_model = "local:embeddinggemma-2:270m".to_string();

    let embedder = app.file_embedder().unwrap();
    assert!(embedder.is_ollama());
    assert!(
        embedder.same_route(
            &crate::provider::embedding::Embedder::ollama(&dead, "embeddinggemma-2:270m".into())
                .unwrap()
        )
    );

    app.rescan_files();
    drain(&mut app).await;
    assert_eq!(
        app.index.embedding_status,
        "embeddings: not reachable — is `ollama serve` running?"
    );
    assert!(stored(&app).is_empty());

    point_at(&mut app, "http://user:secret@127.0.0.1:1");
    assert!(app.file_embedder().is_none());
}

#[tokio::test]
async fn a_corrupt_image_stays_findable_by_name_and_does_not_stall_the_queue() {
    let mock = MockOllama::start(true).await;
    let mut app = app_with_key();
    point_at(&mut app, &mock.endpoint);
    let dir = space_with_files(&app);
    std::fs::write(dir.join("broken.png"), b"\x89PNG\r\n\x1a\ntruncated").unwrap();
    app.index.embedding_model = "local:embeddinggemma-2".to_string();
    app.rescan_files();
    drain(&mut app).await;
    let rows = stored(&app);
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["beach.png", "broken.png", "notes.txt"]
    );
    assert_eq!(
        rows[1].1, "image",
        "falls back to a vector of its name, as an image hit"
    );
    assert_eq!(
        image_inputs(&mock),
        2,
        "the corrupt file never reaches the runtime as an image"
    );
}
