//! View state and UI feedback for the terminal frontend.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use anyhow::{Result, bail};
use ratatui::layout::Rect;
use tui_textarea::TextArea;

use nexus_core::app::{
    App, AppEvent, AppsMode, CopyOption, FilesMode, FilesTab, ImagesMode, ModelPanel, MouseTarget,
    PendingEditor, Popup, ScriptsMode, SessionMode, SettingsField, SkillsMode, SpaceMode,
    SwarmPopupMode,
};
use nexus_core::db::Space as SpaceRow;
use nexus_core::provider::BackendTag;

use crate::filter_input::FilterInput;
use crate::history_cache::HistoryCache;
use crate::selection::HistorySel;
use crate::theme::{BackgroundMode, Theme};
use nexus_core::remote_image::{Fetcher, Status as ImageStatus};
use std::sync::{Arc, Weak};

thread_local! {
    // Layout helpers must not retain a closed view's fetcher.
    static IMAGE_CONTEXT: std::cell::RefCell<Weak<Fetcher>> = const { std::cell::RefCell::new(Weak::new()) };
}

/// Test hook: render on this thread against `fetcher`.
#[cfg(test)]
pub(crate) fn use_fetcher_for_tests(fetcher: &Arc<Fetcher>) {
    IMAGE_CONTEXT.with_borrow_mut(|context| *context = Arc::downgrade(fetcher));
}

pub(crate) fn favicon(origin: &str) -> ImageStatus {
    IMAGE_CONTEXT.with_borrow(|context| {
        context
            .upgrade()
            .map_or(ImageStatus::Off, |fetcher| fetcher.favicon(origin))
    })
}

pub(crate) fn remote_image(url: &str) -> ImageStatus {
    IMAGE_CONTEXT.with_borrow(|context| {
        context
            .upgrade()
            .map_or(ImageStatus::Off, |fetcher| fetcher.request(url))
    })
}

pub(crate) fn image_placeholder(
    url: &str,
    alt: &str,
    status: &ImageStatus,
    theme: &Theme,
    max_width: usize,
) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    let alt = crate::ui::markdown::terminal_safe(alt).into_owned();
    let lead = format!("{} ", crate::ui::style::glyph::MEDIA);
    let full = match status {
        ImageStatus::Loading => " \u{b7} loading".to_string(),
        ImageStatus::Failed => format!(" \u{b7} {host} (couldn't load)"),
        _ => format!(" \u{b7} {host}"),
    };
    let short = match status {
        ImageStatus::Loading => full.clone(),
        ImageStatus::Failed => " \u{b7} couldn't load".to_string(),
        _ => String::new(),
    };
    let count = |text: &str| text.chars().count();
    let room = |suffix: &str| max_width.saturating_sub(count(&lead) + count(suffix));
    let suffix = if count(&alt) <= room(&full) || room(&full) >= 8 {
        full
    } else {
        short
    };
    let budget = room(&suffix);
    let alt = if count(&alt) > budget {
        let keep = budget.saturating_sub(1);
        format!("{}\u{2026}", alt.chars().take(keep).collect::<String>())
    } else {
        alt
    };
    let alt = if budget == 0 { String::new() } else { alt };
    Line::from(vec![
        Span::raw(format!("{lead}{alt}")),
        Span::styled(suffix, ratatui::style::Style::default().fg(theme.fg_dim)),
    ])
}

thread_local! {
    static PIXELS: std::cell::RefCell<HashMap<(std::path::PathBuf, u16, u16), Vec<ratatui::text::Line<'static>>>> =
        std::cell::RefCell::default();
}

fn pixel_lines(
    path: &std::path::Path,
    width: u16,
    rows: u16,
) -> Option<Vec<ratatui::text::Line<'static>>> {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    let key = (path.to_path_buf(), width, rows);
    if let Some(hit) = PIXELS.with_borrow(|cache| cache.get(&key).cloned()) {
        return Some(hit);
    }
    let image = image::open(path).ok()?;
    let pixels = image
        .thumbnail(u32::from(width), u32::from(rows) * 2)
        .to_rgb8();
    let lines: Vec<_> = (0..pixels.height())
        .step_by(2)
        .map(|y| {
            Line::from(
                (0..pixels.width())
                    .map(|x| {
                        let top = pixels.get_pixel(x, y).0;
                        let bottom = pixels.get_pixel(x, (y + 1).min(pixels.height() - 1)).0;
                        Span::styled(
                            "\u{2580}",
                            Style::default()
                                .fg(Color::Rgb(top[0], top[1], top[2]))
                                .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    PIXELS.with_borrow_mut(|cache| {
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, lines.clone());
    });
    Some(lines)
}

/// Reserves the same geometry for loaded, pending and unavailable images.
pub(crate) fn image_lines(
    url: &str,
    alt: &str,
    width: u16,
    rows: u16,
    theme: &Theme,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::Line;
    let rows = rows.min(20);
    let status = remote_image(url);
    let mut lines = Vec::new();
    if let ImageStatus::Ready(path) = &status
        && width > 0
        && rows > 0
        && let Some(pixels) = pixel_lines(path, width, rows)
    {
        lines = pixels;
    } else {
        lines.push(image_placeholder(
            url,
            alt,
            &status,
            theme,
            usize::from(width),
        ));
    }
    lines.resize(usize::from(rows), Line::default());
    lines
}

const BANNER: &str = r"
███╗   ██╗███████╗██╗  ██╗██╗   ██╗███████╗
████╗  ██║██╔════╝╚██╗██╔╝██║   ██║██╔════╝
██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║███████╗
██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║╚════██║
██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝███████║
╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝";

#[allow(clippy::struct_excessive_bools)]
pub struct AppView {
    pub core: App,

    pub input: TextArea<'static>,
    /// X11 clipboard ownership must survive between copy operations.
    pub clipboard: Option<arboard::Clipboard>,
    pub input_inner: Rect,
    pub composer_click: Option<(std::time::Instant, (u16, u16))>,
    pub composer_click_count: u8,
    pub composer_word_anchor: Option<(usize, usize)>,
    pub cmd_selected: usize,
    /// Sent-message offset, with zero referring to the newest message.
    pub recall: Option<usize>,
    pub recall_draft: String,
    /// Autocomplete matches, selected row and byte offset of `@`.
    pub at_state: Option<(Vec<crate::composer::AtMatch>, usize, usize)>,
    /// Workspace root, listing time and paths for `@` completion.
    pub project_files: Option<(
        std::path::PathBuf,
        std::time::Instant,
        std::sync::Arc<Vec<String>>,
    )>,
    pub shell_state: Option<crate::composer::ShellCompletionState>,
    pub shell_completion_rx:
        Option<tokio::sync::oneshot::Receiver<crate::composer::ShellCompletionResult>>,
    pub shell_recall: Option<(Vec<String>, usize, String)>,

    pub popup: Popup,
    pub approval_request: Option<nexus_core::tools::ApprovalRequest>,
    /// Request generation distinguishes identical queued approvals when resetting guards.
    pub approval_displayed: u64,
    pub approval_scroll: std::cell::Cell<usize>,
    pub approval_max_scroll: std::cell::Cell<usize>,
    pub approval_page_height: std::cell::Cell<usize>,
    pub approval_area: std::cell::Cell<Option<ratatui::layout::Rect>>,
    /// Approval remains locked until the final wrapped row has been shown.
    pub approval_seen_end: std::cell::Cell<bool>,
    pub approval_opened_at: Option<std::time::Instant>,
    pub approval_delay: std::time::Duration,
    pub interactive: crate::interactive::Widgets,
    pub spaces_cache: Vec<SpaceRow>,
    pub space_selected: usize,
    pub space_filter: FilterInput,
    pub space_mode: SpaceMode,
    pub space_edit: String,
    pub session_selected: usize,
    pub session_filter: FilterInput,
    pub session_mode: SessionMode,
    pub session_edit: String,
    pub session_preview: Option<(String, String)>,
    pub files_selected: usize,
    pub files_mode: FilesMode,
    pub files_tab: FilesTab,
    pub files_edit: String,
    pub picker_home: PathBuf,
    pub picker_query: String,
    pub picker_results: Vec<nexus_core::filesearch::Found>,
    pub picker_selected: usize,
    pub picker_rx: Option<tokio::sync::oneshot::Receiver<Vec<nexus_core::filesearch::Found>>>,
    pub picker_spotlight: bool,
    pub images_selected: usize,
    pub images_mode: ImagesMode,
    pub scripts_selected: usize,
    pub scripts_mode: ScriptsMode,
    pub scripts_edit: String,
    pub apps_selected: usize,
    pub apps_mode: AppsMode,
    pub apps_edit: String,
    pub pinned_apps: Vec<nexus_core::panels::Pinned>,
    pub swarm_selected: usize,
    pub swarm_popup_mode: SwarmPopupMode,
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    pub skills_edit: String,
    pub usage_data: Option<nexus_core::app::usage::UsageData>,
    pub usage_scroll: usize,
    pub harness_selected: usize,
    pub jobs_popup: crate::ui::popups::jobs::JobsPopup,
    pub library: crate::ui::popups::library::LibraryPopup,
    pub subagents: crate::ui::popups::subagents::SubagentsPopup,
    pub dialogs: Box<dyn nexus_core::canvas::dialogs::Dialogs>,
    pub connectors_popup: crate::ui::popups::connectors::ConnectorsPopup,
    pub procs_selected: usize,
    pub procs_output: Option<String>,
    pub procs_scroll: u16,
    pub procs_kill_confirm: Option<String>,
    pub help_scroll: u16,
    pub key_input: String,
    pub key_target: BackendTag,
    pub login_selected: usize,
    pub local_selected: usize,
    pub local_from_login: bool,
    pub settings_selected: usize,
    pub settings_inputs: [String; 7],
    pub settings_collapsed: HashSet<usize>,
    pub model_filter: FilterInput,
    pub model_backend_filter: Option<BackendTag>,
    pub model_focus: ModelPanel,
    pub fav_selected: usize,
    pub avail_selected: usize,
    pub fav_offset: usize,
    pub avail_offset: usize,
    pub copy_options: Vec<CopyOption>,
    pub copy_selected: usize,

    /// Lines above the bottom, with zero following new output.
    pub scroll: usize,
    pub max_scroll: usize,
    pub history_height: usize,
    pub list_hit: std::cell::Cell<Option<crate::ui::popups::chrome::ListHit>>,
    pub popup_hit: std::cell::Cell<Option<crate::ui::popups::chrome::PopupHit>>,
    pub drag: Option<crate::resize::Drag>,
    pub edge_click: Option<(std::time::Instant, crate::resize::Grip)>,
    pub prev_total: usize,
    /// Streaming rewraps invalidate line indices, so viewport anchors use text.
    pub prev_tail: Vec<String>,
    /// Display toggles must preserve the viewport top even while following a stream.
    pub pin_viewport_top: bool,
    pub sel: HistorySel,
    /// Drag and release stay with the pressed pane even after the pointer leaves it.
    pub mouse_target: MouseTarget,
    pub history_cache: HistoryCache,
    /// Device-local image cache shared across replies, layouts and search cards.
    pub remote_images: Arc<Fetcher>,
    pub stream_image_cache:
        std::cell::RefCell<HashMap<(String, usize), Vec<ratatui::text::Line<'static>>>>,
    pub diagrams: crate::diagrams::DiagramCache,
    pub favicons: crate::favicons::Favicons,
    pub pictures: crate::pictures::Pictures,
    pub image_viewer: crate::image_viewer::Viewer,
    pub session_caches: HashMap<String, HistoryCache>,
    pub show_tool_detail: bool,
    pub tool_open: HashMap<usize, bool>,
    pub batch_open: HashSet<(usize, usize)>,
    pub batch_focus: Option<(usize, usize)>,
    pub tool_open_gen: usize,
    pub studio: crate::studio::Studio,
    pub tool_session: Option<String>,
    pub tool_click_anchor: Option<(usize, Option<usize>, usize)>,
    pub notification_areas: Vec<(Rect, usize)>,
    pub welcome_targets: Vec<(Rect, String)>,
    pub banner: String,
    pub greeting: &'static str,
    pub theme: Theme,
    pub background_mode: BackgroundMode,
    pub vim_enabled: bool,
    pub vim: crate::vim::Vim,
    pub theme_link: String,
    /// Cached lines embed colors and must be invalidated when the theme changes.
    pub theme_gen: usize,

    pub status: String,
    pub status_at: Option<std::time::Instant>,
    pub should_quit: bool,
    pub quit_armed_at: Option<std::time::Instant>,
    /// External edits must run in the event loop, which owns terminal suspension.
    pub pending_editor: Option<PendingEditor>,
}

impl Deref for AppView {
    type Target = App;
    fn deref(&self) -> &App {
        &self.core
    }
}

impl DerefMut for AppView {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.core
    }
}

pub(crate) const MOTION_SETTING: &str = "motion";

pub(crate) fn load_motion_enabled(core: &App) -> bool {
    core.db.load_settings().ok().is_none_or(|settings| {
        settings
            .iter()
            .all(|(key, value)| key != MOTION_SETTING || value != "off")
    })
}

fn load_vim_enabled(core: &App) -> bool {
    core.db.load_settings().ok().is_none_or(|settings| {
        settings
            .iter()
            .all(|(key, value)| key != crate::vim::KEYMAP_SETTING || value != "standard")
    })
}

fn load_background_mode(core: &App) -> BackgroundMode {
    core.db
        .load_settings()
        .ok()
        .and_then(|settings| {
            settings
                .into_iter()
                .find(|(key, _)| key == BackgroundMode::SETTING_KEY)
        })
        .and_then(|(_, value)| BackgroundMode::parse(&value))
        .unwrap_or_default()
}

impl AppView {
    #[allow(clippy::too_many_lines)]
    pub fn new(mut core: App) -> Self {
        core.interactive_ui = true;
        core.interactive_approvals = true;
        core.refresh_toolbox();
        if let Err(error) = core.collect_job_notifications() {
            core.push_status(format!("jobs notifications: {error}"));
        }
        let status = core
            .pop_pending_event()
            .map(|ev| match ev {
                AppEvent::Status(s) => s,
                _ => String::new(),
            })
            .unwrap_or_default();
        let remote_images = Fetcher::new(core.space.db_path().with_file_name("cache"));
        // Tests stay hermetic: the default fetcher never downloads; image tests
        // install their own seeded fetcher with `use_fetcher_for_tests`.
        #[cfg(test)]
        remote_images.set_enabled(false);
        let background_mode = load_background_mode(&core);
        let vim_enabled = load_vim_enabled(&core);
        let mut theme = crate::theme::load();
        theme.set_background_mode(background_mode);
        let mut interactive = crate::interactive::Widgets::default();
        interactive.vim = vim_enabled;
        interactive.motion = load_motion_enabled(&core);
        // Tests stay deterministic: motion follows the wall clock, so app
        // tests render final frames unless they turn it on.
        #[cfg(test)]
        {
            interactive.motion = false;
        }
        Self {
            core,
            input: crate::composer::new_textarea(),
            clipboard: arboard::Clipboard::new().ok(),
            input_inner: Rect::default(),
            composer_click: None,
            composer_click_count: 0,
            composer_word_anchor: None,
            cmd_selected: 0,
            recall: None,
            recall_draft: String::new(),
            at_state: None,
            project_files: None,
            shell_state: None,
            shell_completion_rx: None,
            shell_recall: None,
            popup: Popup::None,
            approval_request: None,
            approval_displayed: 0,
            approval_scroll: std::cell::Cell::new(0),
            approval_area: std::cell::Cell::new(None),
            approval_max_scroll: std::cell::Cell::new(0),
            approval_page_height: std::cell::Cell::new(1),
            approval_seen_end: std::cell::Cell::new(false),
            approval_opened_at: None,
            approval_delay: if cfg!(test) {
                std::time::Duration::ZERO
            } else {
                std::time::Duration::from_millis(400)
            },
            interactive,
            spaces_cache: Vec::new(),
            space_selected: 0,
            space_filter: FilterInput::default(),
            space_mode: SpaceMode::Browse,
            space_edit: String::new(),
            session_selected: 0,
            session_filter: FilterInput::default(),
            session_mode: SessionMode::Browse,
            session_edit: String::new(),
            session_preview: None,
            files_selected: 0,
            files_mode: FilesMode::Browse,
            files_tab: FilesTab::Files,
            files_edit: String::new(),
            picker_home: std::env::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            picker_query: String::new(),
            picker_results: Vec::new(),
            picker_selected: 0,
            picker_rx: None,
            picker_spotlight: cfg!(all(target_os = "macos", not(test))),
            images_selected: 0,
            images_mode: ImagesMode::Browse,
            scripts_selected: 0,
            scripts_mode: ScriptsMode::Browse,
            scripts_edit: String::new(),
            apps_selected: 0,
            apps_mode: AppsMode::Browse,
            apps_edit: String::new(),
            pinned_apps: Vec::new(),
            swarm_selected: 0,
            swarm_popup_mode: SwarmPopupMode::Browse,
            skills_mode: SkillsMode::Browse,
            skills_selected: 0,
            skills_edit: String::new(),
            usage_data: None,
            usage_scroll: 0,
            harness_selected: 0,
            jobs_popup: crate::ui::popups::jobs::JobsPopup::default(),
            library: crate::ui::popups::library::LibraryPopup::default(),
            subagents: crate::ui::popups::subagents::SubagentsPopup::default(),
            dialogs: crate::ui::popups::library::default_dialogs(),
            connectors_popup: crate::ui::popups::connectors::ConnectorsPopup::default(),
            procs_selected: 0,
            procs_output: None,
            procs_scroll: 0,
            procs_kill_confirm: None,
            help_scroll: 0,
            key_input: String::new(),
            key_target: BackendTag::OpenRouter,
            login_selected: 0,
            local_selected: 0,
            local_from_login: false,
            settings_selected: 0,
            settings_inputs: Default::default(),
            settings_collapsed: HashSet::new(),
            model_filter: FilterInput::default(),
            model_backend_filter: None,
            model_focus: ModelPanel::Available,
            fav_selected: 0,
            avail_selected: 0,
            fav_offset: 0,
            avail_offset: 0,
            copy_options: Vec::new(),
            copy_selected: 0,
            scroll: 0,
            max_scroll: 0,
            history_height: 0,
            list_hit: std::cell::Cell::new(None),
            popup_hit: std::cell::Cell::new(None),
            drag: None,
            edge_click: None,
            prev_total: 0,
            prev_tail: Vec::new(),
            pin_viewport_top: false,
            sel: HistorySel::default(),
            mouse_target: MouseTarget::None,
            history_cache: HistoryCache::default(),
            remote_images,
            stream_image_cache: std::cell::RefCell::default(),
            diagrams: crate::diagrams::DiagramCache::default(),
            favicons: crate::favicons::Favicons::default(),
            pictures: crate::pictures::Pictures::default(),
            image_viewer: crate::image_viewer::Viewer::default(),
            session_caches: HashMap::new(),
            show_tool_detail: false,
            tool_open: HashMap::new(),
            batch_open: HashSet::new(),
            batch_focus: None,
            tool_open_gen: 0,
            studio: crate::studio::Studio::default(),
            tool_session: None,
            tool_click_anchor: None,
            notification_areas: Vec::new(),
            welcome_targets: Vec::new(),
            banner: nexus_core::config::load_banner()
                .unwrap_or_else(|| BANNER.trim_matches('\n').to_string()),
            greeting: nexus_core::app::pick_greeting(),
            theme,
            background_mode,
            vim_enabled,
            vim: crate::vim::Vim::default(),
            theme_link: crate::theme::source_stamp(),
            theme_gen: 0,
            status,
            status_at: Some(std::time::Instant::now()),
            should_quit: false,
            quit_armed_at: None,
            pending_editor: None,
        }
    }

    pub fn apply_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::Status(s) => {
                self.status.clone_from(s);
                self.status_at = Some(std::time::Instant::now());
            }
            AppEvent::ComposerSet(s) => self.set_input(s),
            AppEvent::ComposerClear => self.clear_input(),
            AppEvent::ViewportReset => {
                self.shell_state = None;
                self.shell_completion_rx = None;
                self.shell_recall = None;
                self.scroll = 0;
                self.max_scroll = 0;
                self.prev_total = 0;
                self.prev_tail.clear();
                self.tool_open.clear();
                self.batch_open.clear();
                self.batch_focus = None;
                self.tool_open_gen = self.tool_open_gen.wrapping_add(1);
                self.tool_click_anchor = None;
                self.diagrams.clear();
                self.interactive.clear();
                if self.popup == Popup::InteractiveUi {
                    self.popup = Popup::None;
                }
                self.sel.clear();
            }
            AppEvent::HistoryInvalidated => self.invalidate_history_cache(),
            AppEvent::OpenLoginPopup if self.popup != Popup::Approval => self.open_login_popup(),
            _ => {}
        }
        self.sync_approval_popup();
    }

    pub(crate) fn sync_approval_popup(&mut self) {
        if self
            .approvals
            .pending
            .as_ref()
            .is_some_and(tokio::sync::oneshot::Sender::is_closed)
        {
            self.approvals.pending = None;
            self.approvals.pending_request = None;
        }
        self.show_next_approval();
        if let Some(request) = &self.approvals.pending_request {
            if self.popup != Popup::Approval
                || self.approval_request.as_ref() != Some(request)
                || self.approval_displayed != self.approvals.shown
            {
                self.approval_request = Some(request.clone());
                self.approval_displayed = self.approvals.shown;
                self.approval_scroll.set(0);
                self.approval_max_scroll.set(0);
                self.approval_page_height.set(1);
                self.approval_seen_end.set(false);
                self.approval_opened_at = Some(std::time::Instant::now());
                self.popup = Popup::Approval;
            }
        } else if self.popup == Popup::Approval && self.approvals.pending.is_none() {
            self.approval_request = None;
            self.popup = Popup::None;
        }
    }

    pub fn set_background_mode(&mut self, requested: &str) -> Result<()> {
        let requested = requested.trim();
        let mode = if requested.is_empty() {
            self.background_mode.next()
        } else {
            let Some(mode) = BackgroundMode::parse(requested) else {
                bail!(
                    "unknown theme background {requested:?} — use /theme opaque or /theme transparent"
                );
            };
            mode
        };
        self.core
            .db
            .set_setting(BackgroundMode::SETTING_KEY, mode.key())?;
        self.background_mode = mode;
        self.theme.set_background_mode(mode);
        self.theme_gen = self.theme_gen.wrapping_add(1);
        self.invalidate_history_cache();
        self.push_status(format!("theme background: {}", mode.label()));
        Ok(())
    }

    pub fn set_keymap(&mut self, requested: &str) -> Result<()> {
        let enabled = match requested.trim() {
            "" => !self.vim_enabled,
            "on" | "vim" => true,
            "off" | "standard" => false,
            other => bail!("unknown keymap {other:?} — use /vim on or /vim off"),
        };
        self.core.db.set_setting(
            crate::vim::KEYMAP_SETTING,
            if enabled { "vim" } else { "standard" },
        )?;
        self.vim_enabled = enabled;
        self.interactive.vim = enabled;
        self.vim.reset();
        self.push_status(format!("vim keys: {}", if enabled { "on" } else { "off" }));
        Ok(())
    }

    pub fn set_motion(&mut self, requested: &str) -> Result<()> {
        let enabled = match requested.trim() {
            "" => !self.interactive.motion,
            "on" => true,
            "off" => false,
            other => bail!("unknown motion setting {other:?} — use /motion on or /motion off"),
        };
        self.core
            .db
            .set_setting(MOTION_SETTING, if enabled { "on" } else { "off" })?;
        self.interactive.motion = enabled;
        self.push_status(format!("motion: {}", if enabled { "on" } else { "off" }));
        Ok(())
    }

    pub(crate) fn activate_image_context(&self) -> usize {
        self.remote_images
            .set_enabled(self.core.settings.remote_images);
        IMAGE_CONTEXT.with_borrow_mut(|context| *context = Arc::downgrade(&self.remote_images));
        self.remote_images.generation()
    }

    pub fn invalidate_history_cache(&mut self) {
        if let Some(sid) = self.core.session.as_ref().map(|s| s.id.clone()) {
            self.session_caches.remove(&sid);
        }
        self.history_cache = HistoryCache::default();
    }

    pub fn open_session_link(&mut self) {
        let owner = self.sel.owner_at_selection_start();
        self.core.open_session_link(owner);
    }

    pub fn copy_text(&mut self, text: &str) {
        let msg = crate::composer::copy_to_clipboard(&mut self.clipboard, text);
        if !msg.is_empty() {
            self.push_status(msg);
        }
    }

    /// Copies original message text, using `messages.len()` for the streaming reply.
    pub fn copy_message(&mut self, idx: usize) {
        let text = match self.core.messages.get(idx) {
            Some(m) if m.role == "assistant" => Some(nexus_core::markdown::to_plain(&m.content)),
            Some(m) => Some(m.content.clone()),
            None if idx == self.core.messages.len() => {
                self.core.active_streaming_text().map(str::to_string)
            }
            None => None,
        };
        if let Some(t) = text {
            self.copy_text(&t);
        }
    }

    pub fn text_index(&self) -> Option<usize> {
        match self.settings_field()? {
            SettingsField::RemoteImages
            | SettingsField::ShowStats
            | SettingsField::ShowReasoning
            | SettingsField::HideHints
            | SettingsField::Verbosity
            | SettingsField::SearchProvider
            | SettingsField::ReplyStyle
            | SettingsField::ImageViewer
            | SettingsField::CacheLifetime
            | SettingsField::CacheLongOpenai
            | SettingsField::ColumnWidth => None,
            SettingsField::Temperature => Some(0),
            SettingsField::TopP => Some(1),
            SettingsField::MaxTokens => Some(2),
            SettingsField::CompactThreshold => Some(3),
            SettingsField::SearxngUrl => Some(4),
            SettingsField::LangsearchKey => Some(5),
            SettingsField::BlockedDomains => Some(6),
        }
    }

    pub fn open_research_live(&mut self) {
        self.core.research.live_input.clear();
        self.popup = Popup::ResearchLive;
    }
}

pub const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(10);
pub const ERROR_STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

impl AppView {
    pub fn status_deadline(&self) -> Option<std::time::Instant> {
        let at = self.status_at?;
        let lower = self.status.to_lowercase();
        let ttl = if lower.contains("error") || lower.contains("failed") {
            ERROR_STATUS_TTL
        } else {
            STATUS_TTL
        };
        Some(at + ttl)
    }

    pub fn expire_status(&mut self) {
        if self
            .status_deadline()
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.status.clear();
            self.status_at = None;
        }
    }
}

impl AppView {
    pub fn history_page(&self) -> usize {
        self.history_height.saturating_sub(2).max(1)
    }
}

impl AppView {
    pub fn open_welcome_session(&mut self, n: usize) -> anyhow::Result<()> {
        let Some((_, id)) = self.welcome_targets.get(n).cloned() else {
            return Ok(());
        };
        self.core.switch_to_session_by_id(&id)
    }
}
