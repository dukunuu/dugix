mod app_view;
mod cli;
mod cli_library;
mod composer;
mod connectors_cli;
mod diagrams;
mod editor;
mod events;
mod favicons;
mod filter_input;
mod flows;
mod history_cache;
mod image_viewer;
mod image_viewer_actions;
mod interactive;
mod pictures;
mod resize;
mod selection;
mod studio;
mod theme;
mod ui;
mod vim;

use anyhow::Result;
use nexus_core::config;

use app_view::AppView;

#[tokio::main]
async fn main() -> Result<()> {
    let (continue_session, selection_chat, send_only, command) = cli::parse();
    if let Some(cmd) = command {
        if continue_session || selection_chat || send_only {
            anyhow::bail!("launch flags can only be used when launching the TUI");
        }
        return cli::run(cmd).await;
    }

    let selection_prompt = (selection_chat || send_only)
        .then(cli::primary_selection_prompt)
        .flatten();
    if (selection_chat || send_only) && cli::send_selection_request(selection_prompt.as_deref()) {
        return Ok(());
    }
    if send_only {
        anyhow::bail!("no running Nexus TUI instance");
    }

    let saved = config::load_all_providers()?;
    let app = nexus_core::boot(saved).await?;
    let mut app = AppView::new(app);
    let (selection_server, selection_requests) = cli::start_selection_server()?;

    let mut terminal = ratatui::init();
    diagrams::protect_terminal_from_renderer_panics();
    // Graphics probing must precede EventStream taking ownership of terminal input.
    let picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());
    app.diagrams.set_picker(picker.clone());
    app.favicons.set_picker(picker.clone());
    app.pictures.set_picker(picker.clone());
    app.image_viewer.set_picker(picker);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableFocusChange
    );
    // Most terminals otherwise report Backspace and Ctrl+Backspace as the same byte.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
        let _ = crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    app.init();
    open_start_session(&mut app, selection_prompt, continue_session, selection_chat)?;
    app.spawn_update_check();
    app.refresh_codex_in_background();
    let result = events::run(app, &mut terminal, selection_requests).await;
    #[allow(clippy::drop_non_drop)]
    drop(selection_server);
    if enhanced {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableFocusChange
    );
    ratatui::restore();
    result
}

fn open_start_session(
    app: &mut AppView,
    selection_prompt: Option<String>,
    continue_session: bool,
    selection_chat: bool,
) -> Result<()> {
    if let Some(prompt) = selection_prompt {
        app.execute(nexus_core::app::AppCommand::Send { text: prompt })?;
        cli::clear_primary_selection();
    } else if continue_session || selection_chat {
        // Explicit launch flags take precedence over a stale session handoff.
        let _ = std::fs::remove_file(app.space.root.join("pending-open"));
        if let Some((space_id, session)) = app.db.latest_session()? {
            if app.active_space.id != space_id
                && let Some(row) = app.db.list_spaces()?.into_iter().find(|s| s.id == space_id)
            {
                app.set_active_space(row);
            }
            let _ = app.execute(nexus_core::app::AppCommand::ResolveSession { id: session.id });
        } else {
            app.push_status("no previous session — send a message to start one".to_string());
        }
    } else {
        let handoff = app.space.root.join("pending-open");
        if let Ok(id) = std::fs::read_to_string(&handoff) {
            let _ = std::fs::remove_file(&handoff);
            let id = id.trim();
            if !id.is_empty()
                && let Ok((space_id, session)) = cli::resolve_session(&app.db, id)
                && let Ok(rows) = app.db.list_spaces()
                && let Some(row) = rows.into_iter().find(|s| s.id == space_id)
            {
                app.set_active_space(row);
                let _ = app.execute(nexus_core::app::AppCommand::ResolveSession { id: session.id });
            }
        }
    }
    Ok(())
}
