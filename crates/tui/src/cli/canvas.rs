//! `nexus design`, `nexus deck` and `nexus doc` file commands, plus the
//! studio launcher.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, ensure};

use super::{DeckCmd, DesignCmd, DocCmd, out};

pub(super) fn design_files(cmd: DesignCmd, files: &Path) -> Result<()> {
    use nexus_core::design;
    match cmd {
        DesignCmd::Import { path, name } => {
            let path = if path.exists() {
                path
            } else {
                files.join(path)
            };
            let imported = design::template::import_file(files, &path, name.as_deref())?;
            out(format!("design/{}.design.json", imported.slug));
            for warning in &imported.warnings {
                out(format!("warning: {warning}"));
            }
            if let Some(sheet) = design::template::contact_sheet(files, &imported)? {
                out(sheet.display());
            }
        }
        DesignCmd::List => {
            for slug in design::list(files)? {
                let design = design::load(files, &slug)?;
                out(format!(
                    "{slug} · {}",
                    design
                        .source
                        .map_or_else(|| "built-in/space".into(), |s| s.template)
                ));
            }
        }
    }
    Ok(())
}

fn deck_import(path: &Path, out_path: Option<&Path>, files: &Path) -> Result<()> {
    use nexus_core::deck::import;
    let source = if path.exists() {
        path.to_path_buf()
    } else {
        files.join(path)
    };
    let (deck_path, warnings, slides) = if let Some(target) = out_path {
        let target = if target.is_absolute() {
            target.to_path_buf()
        } else {
            files.join(target)
        };
        ensure!(
            target
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".deck.json")),
            "--out must end in .deck.json"
        );
        ensure!(
            !target.exists(),
            "{} already exists; imports never overwrite a deck",
            target.display()
        );
        let imported = import::import_pptx_to(&source, &target, None)?;
        (target, imported.warnings, imported.deck.slides.len())
    } else {
        let imported = import::import_pptx_file(&source, &files.join("decks"), None)?;
        (imported.path, imported.warnings, imported.deck.slides.len())
    };
    out(deck_path.display());
    out(format!(
        "{slides} slide{}",
        if slides == 1 { "" } else { "s" }
    ));
    for warning in &warnings {
        out(format!("warning: {warning}"));
    }
    Ok(())
}

pub(super) fn deck_export(cmd: DeckCmd, files: &Path) -> Result<()> {
    use nexus_core::deck::{Deck, export};
    let (path, format, destination, template) = match cmd {
        DeckCmd::Export {
            path,
            format,
            out,
            template,
        } => (path, format, out, template),
        DeckCmd::Import { path, out } => return deck_import(&path, out.as_deref(), files),
    };
    let path = if path.is_absolute() {
        path
    } else {
        files.join(path)
    };
    let source = std::fs::read_to_string(&path)?;
    let deck = Deck::check(&source).map_err(|errors| {
        anyhow!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    let assets = path.parent().context("deck folder")?.join("assets");
    let output = nexus_core::design::template::export_deck(
        &deck,
        Some(&assets),
        files,
        format.parse()?,
        template.as_deref(),
    )?;
    let destination = destination
        .as_deref()
        .map(std::path::absolute)
        .transpose()?;
    let written = export::write_export(&output, files, destination.as_deref())?;
    out(written.display());
    for warning in &output.warnings {
        out(format!("warning: {warning}"));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn doc_files(cmd: DocCmd, files: &Path) -> Result<()> {
    use nexus_core::{
        deck::export::write_export,
        doc::{self, Doc},
    };
    match cmd {
        DocCmd::Export {
            path,
            format,
            out: destination,
            template,
        } => {
            let path = if path.is_absolute() {
                path
            } else {
                files.join(path)
            };
            let doc = Doc::check(&std::fs::read_to_string(&path)?).map_err(|e| anyhow!("{e:?}"))?;
            let assets = path.parent().context("doc folder")?.join("assets");
            let mut exported = doc.clone();
            let format = format.parse()?;
            if format == doc::export::Format::Md {
                let parent = destination
                    .as_deref()
                    .map(std::path::absolute)
                    .transpose()?
                    .and_then(|p| p.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| files.join("exports"));
                let target = parent.join("assets");
                for block in &mut exported.blocks {
                    if let doc::Kind::Image { src, .. } = &mut block.kind
                        && !src.starts_with("https://")
                    {
                        let image = assets.join(src.strip_prefix("assets/").unwrap_or(src));
                        ensure!(
                            std::fs::metadata(&image)?.len()
                                <= nexus_core::deck::assets::MAX_IMAGE_BYTES,
                            "image exceeds 16 MB"
                        );
                        *src = nexus_core::deck::assets::store_image_bytes(
                            &std::fs::read(image)?,
                            src,
                            "image",
                            &target,
                        )?;
                    }
                }
            }
            let output = nexus_core::design::template::export_doc(
                &exported,
                Some(&assets),
                &doc::feedback::load(&path)?,
                files,
                format,
                template.as_deref(),
            )?;
            let destination = destination
                .as_deref()
                .map(std::path::absolute)
                .transpose()?;
            let written = write_export(&output, files, destination.as_deref())?;
            out(written.display());
            for warning in output.warnings {
                out(format!("warning: {warning}"));
            }
        }
        DocCmd::Import {
            path,
            out: destination,
        } => {
            let source = if path.exists() {
                path
            } else {
                files.join(path)
            };
            let target = if let Some(target) = destination {
                if target.is_absolute() {
                    target
                } else {
                    files.join(target)
                }
            } else {
                let stem = source.file_stem().unwrap_or_default().to_string_lossy();
                let stem = stem
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>();
                let stem = if stem.is_empty() {
                    "imported".to_string()
                } else {
                    stem
                };
                let mut target = files.join(format!("docs/{stem}.doc.json"));
                let mut index = 2;
                while target.exists() {
                    target = files.join(format!("docs/{stem}-{index}.doc.json"));
                    index += 1;
                }
                target
            };
            ensure!(
                !target.exists(),
                "{} already exists; imports never overwrite a doc",
                target.display()
            );
            ensure!(
                target.to_string_lossy().ends_with(".doc.json"),
                "--out must end in .doc.json"
            );
            let assets = target.parent().context("doc folder")?.join("assets");
            let imported = doc::import::file(&source, &assets)?;
            doc::import::write_new(&imported, &target)?;
            out(target.display());
            out(format!("{} blocks", imported.doc.blocks.len()));
            for warning in imported.warnings {
                out(format!("warning: {warning}"));
            }
        }
    }
    Ok(())
}

pub(super) fn launch_studio(path: Option<&Path>) -> Result<()> {
    let name = if cfg!(windows) {
        "nexus-studio.exe"
    } else {
        "nexus-studio"
    };
    let beside = std::env::current_exe()?.with_file_name(name);
    for executable in [beside, PathBuf::from(name)] {
        let mut command = std::process::Command::new(executable);
        if let Some(path) = path {
            command.arg(path);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0000_0008 | 0x0000_0200);
        }
        match command.spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    eprintln!("nexus studio isn't installed — cargo install --path crates/studio");
    Ok(())
}
