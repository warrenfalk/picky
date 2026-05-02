use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, ModuleRegistry, ResultAction, SearchResult,
};
use crate::modules;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use iced::advanced::widget::{Operation, operate, operation::Outcome};
use iced::event;
use iced::keyboard::{self, Key, key::Named};
use iced::system;
use iced::widget::operation::{AbsoluteOffset, focus, focus_next, scroll_to};
use iced::widget::scrollable::Viewport;
use iced::widget::{
    Id, button, column, container, image, keyed_column, lazy, row, scrollable, svg, text,
    text_input,
};
use iced::{
    Alignment, Background, Color, Element, Length, Rectangle, Shadow, Size, Subscription, Task,
    Theme, Vector, border, theme, window,
};
use serde::Deserialize;
use serde_json::json;

const WINDOW_WIDTH: f32 = 820.0;
// We intentionally start taller and avoid runtime `window::resize(...)`.
// Resizing after the window opens appears to change the surface size without
// recomputing layout, which is believed to be an Iced 0.14.0 bug.
const DEFAULT_WINDOW_HEIGHT: f32 = 1368.0;
const RESULT_ICON_SIZE: f32 = 28.0;
const SUBTITLE_ICON_SIZE: f32 = 20.0;
const WINDOW_THUMBNAIL_WIDTH: f32 = 96.0;
const WINDOW_THUMBNAIL_HEIGHT: f32 = 60.0;
const WINDOW_THUMBNAIL_MAX_WIDTH: u32 = 256;
const WINDOW_THUMBNAIL_MAX_HEIGHT: u32 = 160;
const WINDOW_THUMBNAIL_OVERSCAN: f32 = 160.0;
const NIRI_THUMBNAIL_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_WINDOW_THUMBNAIL_REQUESTS_PER_BATCH: usize = 6;
const RESULTS_SCROLL_MARGIN: f32 = 8.0;
const SHELL_RADIUS: f32 = 28.0;
const CARD_RADIUS: f32 = 16.0;
const CHIP_RADIUS: f32 = 999.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusTarget {
    Search,
    Results,
}

#[derive(Clone, Debug)]
enum AppKey {
    Down,
    Up,
    Tab,
    Enter,
    Escape,
    FocusSearch,
    Shortcut(char),
}

#[derive(Clone, Debug)]
enum Message {
    QueryChanged(String),
    ActivateSelected,
    ActivateSelectedAction(&'static str),
    ResultActivated(usize),
    ResultsScrolled(Viewport),
    EnsureSelectedResultVisible(u64),
    SelectedResultVisibilityMeasured {
        request_id: u64,
        measured: MeasuredSelectedResultVisibility,
    },
    KeyPressed(AppKey),
    SystemInfoLoaded(system::Information),
    StartupFinished(StartupContext),
    IconIndexFinished(Arc<IconIndex>),
    SearchFinished {
        request_id: u64,
        result: Result<Vec<SearchResult>, String>,
    },
    WindowThumbnailFinished {
        window_id: u64,
        result: Result<LoadedWindowThumbnail, String>,
    },
    ActivationFinished(Result<ActivationOutcome, String>),
}

pub struct PickerApp {
    registry: Option<Arc<Mutex<ModuleRegistry>>>,
    icon_index: Option<Arc<IconIndex>>,
    query: String,
    error_message: String,
    renderer_warning: String,
    results: Vec<SearchResult>,
    selected_index: Option<usize>,
    focus_target: FocusTarget,
    search_request_id: u64,
    active_search_request_id: u64,
    search_input_id: Id,
    results_scroll_id: Id,
    results_viewport: Option<Viewport>,
    results_row_bounds: HashMap<Id, CachedRowBounds>,
    visible_result_row_ids: HashSet<Id>,
    window_thumbnails: HashMap<u64, WindowThumbnailState>,
    selected_result_visibility_request_id: u64,
    is_starting_up: bool,
    is_loading_icon_index: bool,
}

#[derive(Clone)]
struct StartupContext {
    registry: Arc<Mutex<ModuleRegistry>>,
}

impl std::fmt::Debug for StartupContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartupContext")
            .field("registry", &"<module-registry>")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WindowThumbnailState {
    Loading,
    Ready(PathBuf),
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoadedWindowThumbnail {
    window_id: u64,
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
enum NiriIpcReply {
    Ok(NiriIpcResponse),
    Err(String),
}

#[derive(Debug, Deserialize)]
enum NiriIpcResponse {
    WindowThumbnail(NiriWindowThumbnail),
}

#[derive(Debug, Deserialize)]
struct NiriWindowThumbnail {
    id: u64,
    png_base64: String,
}

pub fn run() -> iced::Result {
    iced::application(initialize, update, view)
        .title("Picky")
        .subscription(subscription)
        .theme(theme)
        .style(application_style)
        .window(window::Settings {
            size: Size::new(WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT),
            position: window::Position::Centered,
            decorations: false,
            resizable: false,
            transparent: true,
            platform_specific: window::settings::PlatformSpecific {
                application_id: "picky".to_string(),
                ..window::settings::PlatformSpecific::default()
            },
            ..window::Settings::default()
        })
        .run()
}

fn theme(_app: &PickerApp) -> Theme {
    Theme::custom(
        "Picky Control Center",
        theme::Palette {
            background: color_from_hex(0x151A2D),
            text: color_from_hex(0xE5EAFE),
            primary: color_from_hex(0x86A8FF),
            success: color_from_hex(0xA8D469),
            warning: color_from_hex(0xC3A2FF),
            danger: color_from_hex(0xE18497),
        },
    )
}

fn application_style(_app: &PickerApp, _theme: &Theme) -> theme::Style {
    theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme_text(),
    }
}

fn initialize() -> (PickerApp, Task<Message>) {
    let search_input_id = Id::unique();
    let results_scroll_id = Id::new("results-scroll");
    let app = PickerApp {
        registry: None,
        icon_index: None,
        query: String::new(),
        error_message: String::new(),
        renderer_warning: String::new(),
        results: Vec::new(),
        selected_index: None,
        focus_target: FocusTarget::Search,
        search_request_id: 0,
        active_search_request_id: 0,
        search_input_id,
        results_scroll_id,
        results_viewport: None,
        results_row_bounds: HashMap::new(),
        visible_result_row_ids: HashSet::new(),
        window_thumbnails: HashMap::new(),
        selected_result_visibility_request_id: 0,
        is_starting_up: true,
        is_loading_icon_index: false,
    };

    let task = Task::batch([
        focus(app.search_input_id.clone()),
        system::information().map(Message::SystemInfoLoaded),
        Task::perform(async { load_startup_context() }, Message::StartupFinished),
    ]);

    (app, task)
}

fn subscription(app: &PickerApp) -> Subscription<Message> {
    match app.focus_target {
        FocusTarget::Search => event::listen_with(search_event_message),
        FocusTarget::Results => event::listen_with(results_event_message),
    }
}

fn search_event_message(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };

    search_key_message(key, modifiers)
}

fn results_event_message(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };

    results_key_message(key, modifiers)
}

fn search_key_message(key: Key, _modifiers: keyboard::Modifiers) -> Option<Message> {
    match key {
        Key::Named(Named::ArrowDown) => Some(Message::KeyPressed(AppKey::Down)),
        Key::Named(Named::Tab) => Some(Message::KeyPressed(AppKey::Tab)),
        Key::Named(Named::Escape) => Some(Message::KeyPressed(AppKey::Escape)),
        _ => None,
    }
}

fn results_key_message(key: Key, _modifiers: keyboard::Modifiers) -> Option<Message> {
    match key {
        Key::Named(Named::ArrowDown) => Some(Message::KeyPressed(AppKey::Down)),
        Key::Named(Named::ArrowUp) => Some(Message::KeyPressed(AppKey::Up)),
        Key::Named(Named::Tab) => Some(Message::KeyPressed(AppKey::Tab)),
        Key::Named(Named::Enter) => Some(Message::KeyPressed(AppKey::Enter)),
        Key::Named(Named::Escape) => Some(Message::KeyPressed(AppKey::Escape)),
        Key::Character(value) if value == "/" => Some(Message::KeyPressed(AppKey::FocusSearch)),
        Key::Character(value) => value
            .chars()
            .next()
            .map(|character| Message::KeyPressed(AppKey::Shortcut(character))),
        _ => None,
    }
}

fn update(app: &mut PickerApp, message: Message) -> Task<Message> {
    match message {
        Message::QueryChanged(query) => {
            app.query = query;
            app.focus_target = FocusTarget::Search;
            app.invalidate_selected_result_scroll();
            app.request_search()
        }
        Message::ActivateSelected => app.activate_selected(DEFAULT_ACTION_ID),
        Message::ActivateSelectedAction(action_id) => app.activate_selected(action_id),
        Message::ResultActivated(index) => {
            app.selected_index = Some(index);
            app.focus_target = FocusTarget::Results;
            app.activate_selected(DEFAULT_ACTION_ID)
        }
        Message::ResultsScrolled(viewport) => {
            app.results_viewport = Some(viewport);
            app.visible_result_row_ids = app.visible_result_row_ids_for_viewport(viewport);
            app.request_visible_window_thumbnails()
        }
        Message::EnsureSelectedResultVisible(request_id) => {
            app.measure_selected_result_visibility(request_id)
        }
        Message::SelectedResultVisibilityMeasured {
            request_id,
            measured,
        } => {
            if request_id != app.selected_result_visibility_request_id {
                return Task::none();
            }

            app.results_row_bounds = measured.row_bounds;
            app.visible_result_row_ids = measured.visible_row_ids.into_iter().collect();

            let scroll_task = measured
                .target_offset_y
                .map_or_else(Task::none, |offset_y| {
                    scroll_to(
                        app.results_scroll_id.clone(),
                        AbsoluteOffset {
                            x: 0.0,
                            y: offset_y,
                        },
                    )
                });
            let thumbnail_task = app.request_visible_window_thumbnails();

            Task::batch([scroll_task, thumbnail_task])
        }
        Message::SystemInfoLoaded(info) => {
            app.renderer_warning =
                tiny_skia_warning(&info).map_or_else(String::new, str::to_string);
            Task::none()
        }
        Message::StartupFinished(startup) => {
            app.is_starting_up = false;
            app.registry = Some(startup.registry);
            app.request_search()
        }
        Message::IconIndexFinished(icon_index) => {
            app.is_loading_icon_index = false;
            app.icon_index = Some(icon_index);
            Task::none()
        }
        Message::KeyPressed(key) => match app.focus_target {
            FocusTarget::Search => app.handle_search_key(key),
            FocusTarget::Results => app.handle_results_key(key),
        },
        Message::SearchFinished { request_id, result } => {
            if request_id != app.active_search_request_id {
                return Task::none();
            }

            match result {
                Ok(results) => {
                    app.error_message.clear();
                    app.results = results;
                    app.results_row_bounds.clear();
                    app.visible_result_row_ids.clear();
                    app.selected_index = (!app.results.is_empty()).then_some(0);
                    let icon_index_task = app.request_icon_index();

                    if app.focus_target == FocusTarget::Results && !app.results.is_empty() {
                        Task::batch([
                            focus_first_result(&app.search_input_id),
                            app.schedule_selected_result_scroll(),
                            icon_index_task,
                        ])
                    } else if !app.results.is_empty() {
                        Task::batch([app.schedule_selected_result_scroll(), icon_index_task])
                    } else if app.results.is_empty() && app.focus_target == FocusTarget::Results {
                        app.focus_target = FocusTarget::Search;
                        app.invalidate_selected_result_scroll();
                        Task::batch([focus(app.search_input_id.clone()), icon_index_task])
                    } else {
                        app.invalidate_selected_result_scroll();
                        icon_index_task
                    }
                }
                Err(error_message) => {
                    app.error_message = error_message;
                    app.results.clear();
                    app.selected_index = None;
                    app.invalidate_selected_result_scroll();

                    if app.focus_target == FocusTarget::Results {
                        app.focus_target = FocusTarget::Search;
                        focus(app.search_input_id.clone())
                    } else {
                        Task::none()
                    }
                }
            }
        }
        Message::WindowThumbnailFinished { window_id, result } => {
            match result {
                Ok(thumbnail) if thumbnail.window_id == window_id => {
                    app.window_thumbnails
                        .insert(window_id, WindowThumbnailState::Ready(thumbnail.path));
                }
                Ok(thumbnail) => {
                    eprintln!(
                        "niri returned thumbnail for window {}, expected {window_id}",
                        thumbnail.window_id
                    );
                    app.window_thumbnails
                        .insert(window_id, WindowThumbnailState::Failed);
                }
                Err(error) => {
                    eprintln!("failed to load thumbnail for window {window_id}: {error}");
                    app.window_thumbnails
                        .insert(window_id, WindowThumbnailState::Failed);
                }
            }

            app.request_visible_window_thumbnails()
        }
        Message::ActivationFinished(result) => match result {
            Ok(ActivationOutcome::ClosePicker) => iced::exit(),
            Ok(ActivationOutcome::RefreshResults) => app.request_search(),
            Err(error_message) => {
                app.error_message = error_message;
                Task::none()
            }
        },
    }
}

fn view(app: &PickerApp) -> Element<'_, Message> {
    let search = text_input("Type to search", &app.query)
        .id(app.search_input_id.clone())
        .on_input(Message::QueryChanged)
        .on_submit(Message::ActivateSelected)
        .padding([14, 16])
        .size(22)
        .style(search_input_style);

    let results_list: Element<'_, Message> = if app.results.is_empty() {
        let (title, subtitle) = if app.is_starting_up {
            (
                "Loading results",
                "Picker is starting up. You can type now and results will populate shortly.",
            )
        } else {
            (
                "No matches",
                "Refine the query or switch focus with the arrow keys.",
            )
        };

        container(
            column![
                text(title).size(22).color(theme_text()),
                text(subtitle).size(14).color(theme_secondary_text()),
            ]
            .spacing(6),
        )
        .padding(20)
        .style(panel_card_style)
        .width(Length::Fill)
        .into()
    } else {
        let rows = app
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let row = ResultRowView {
                    index,
                    result: result.clone(),
                    icon_path: resolve_icon_path(
                        result.icon_name.as_deref(),
                        app.icon_index.as_deref(),
                    ),
                    window_thumbnail_path: window_thumbnail_path_for_result(
                        result,
                        &app.window_thumbnails,
                    ),
                    is_selected: app.selected_index == Some(index),
                    show_action_hints: app.selected_index == Some(index)
                        && app.focus_target == FocusTarget::Results,
                };

                (row_key(&row.result), lazy(row, view_result_row).into())
            })
            .collect::<Vec<_>>();

        keyed_column(rows).spacing(6).width(Length::Fill).into()
    };

    let search_panel = container(search).padding(18).style(panel_card_style);

    let mut content = column![
        search_panel,
        text(format!("{} results", app.results.len()))
            .size(14)
            .color(theme_secondary_text()),
        container(
            scrollable(results_list)
                .id(app.results_scroll_id.clone())
                .on_scroll(Message::ResultsScrolled)
                .style(results_scrollable_style)
                .height(Length::Fill),
        )
        .padding(10)
        .height(Length::Fill)
        .style(results_surface_style),
    ]
    .spacing(14)
    .height(Length::Fill);

    if !app.renderer_warning.is_empty() {
        content = content.push(view_status_banner(
            "Renderer",
            app.renderer_warning.clone(),
            BannerTone::Warning,
        ));
    }

    if !app.error_message.is_empty() {
        content = content.push(view_status_banner(
            "Error",
            app.error_message.clone(),
            BannerTone::Danger,
        ));
    }

    container(
        container(content)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(shell_style),
    )
    .padding(20)
    .width(Length::Fill)
    .height(Length::Fill)
    .style(app_background_style)
    .into()
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ResultRowView {
    index: usize,
    result: SearchResult,
    icon_path: Option<PathBuf>,
    window_thumbnail_path: Option<PathBuf>,
    is_selected: bool,
    show_action_hints: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BannerTone {
    Warning,
    Danger,
}

fn view_result_row(row_state: &ResultRowView) -> Element<'static, Message> {
    let index = row_state.index;
    let is_selected = row_state.is_selected;
    let show_action_hints = row_state.show_action_hints;
    let result = &row_state.result;
    let icon_path = row_state.icon_path.clone();
    let window_thumbnail_path = row_state.window_thumbnail_path.clone();
    let row_id = row_widget_id(result);
    let title_color = if is_selected {
        color_from_hex(0xF7FAFF)
    } else {
        theme_text()
    };
    let subtitle_color = if is_selected {
        color_from_hex(0xD9E3FF)
    } else {
        theme_secondary_text()
    };

    let mut text_column =
        column![text(result.title.clone()).size(18).color(title_color)].spacing(6);

    if !result.subtitle.trim().is_empty() {
        let subtitle_line = row![text(result.subtitle.clone()).size(14).color(subtitle_color)];

        text_column = text_column.push(subtitle_line);
    }

    if show_action_hints && !result.actions.is_empty() {
        let action_row = result
            .actions
            .iter()
            .fold(row![].spacing(8), |row, action| {
                row.push(view_action_chip(action, is_selected))
            });

        text_column = text_column.push(action_row);
    }

    let row_content = row![
        leading_visual(result, icon_path, window_thumbnail_path, is_selected),
        text_column.width(Length::Fill)
    ]
    .align_y(Alignment::Center)
    .spacing(12)
    .width(Length::Fill);

    container(
        button(container(row_content).width(Length::Fill))
            .width(Length::Fill)
            .padding(12)
            .style(move |theme, status| result_row_button_style(theme, status, is_selected))
            .on_press(Message::ResultActivated(index)),
    )
    .id(row_id)
    .width(Length::Fill)
    .into()
}

fn row_key(result: &SearchResult) -> u64 {
    let mut hasher = DefaultHasher::new();
    result.module_key.hash(&mut hasher);
    result.item_id.hash(&mut hasher);
    hasher.finish()
}

fn row_widget_id(result: &SearchResult) -> Id {
    Id::from(format!(
        "result-row:{}:{}",
        result.module_key, result.item_id
    ))
}

fn result_row_button_style(
    _theme: &Theme,
    status: button::Status,
    is_selected: bool,
) -> button::Style {
    if is_selected {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => color_from_hex(0x6D8FF5),
            button::Status::Active | button::Status::Disabled => color_from_hex(0x5E79D8),
        };

        return button::Style {
            background: Some(Background::Color(background)),
            text_color: color_from_hex(0xF7FAFF),
            border: border::rounded(CARD_RADIUS)
                .width(1)
                .color(color_from_hex(0x97B6FF)),
            shadow: Shadow {
                color: color_from_rgba_hex(0x080D16, 0.20),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 18.0,
            },
            ..button::Style::default()
        };
    }

    let background = match status {
        button::Status::Hovered => color_from_hex(0x2D3550),
        button::Status::Pressed => color_from_hex(0x333D5B),
        button::Status::Active | button::Status::Disabled => theme_card_surface(),
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: theme_text(),
        border: border::rounded(CARD_RADIUS).width(1).color(theme_border()),
        shadow: Shadow {
            color: color_from_rgba_hex(0x050913, 0.18),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..button::Style::default()
    }
}

fn tiny_skia_warning(info: &system::Information) -> Option<&'static str> {
    info.graphics_backend
        .eq_ignore_ascii_case("tiny-skia")
        .then_some("Using software renderer (tiny-skia); performance may be degraded.")
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScrollableSnapshot {
    bounds: Rectangle,
    content_bounds: Rectangle,
    offset: Vector,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CachedRowBounds {
    top: f32,
    height: f32,
}

#[derive(Clone, Debug)]
struct MeasuredSelectedResultVisibility {
    row_bounds: HashMap<Id, CachedRowBounds>,
    visible_row_ids: HashSet<Id>,
    target_offset_y: Option<f32>,
}

#[derive(Debug)]
struct MeasureSelectedResultVisibilityOperation {
    scrollable_id: Id,
    tracked_row_ids: HashSet<Id>,
    row_id: Id,
    scrollable: Option<ScrollableSnapshot>,
    row_bounds: HashMap<Id, Rectangle>,
}

impl MeasureSelectedResultVisibilityOperation {
    fn new(scrollable_id: Id, tracked_row_ids: HashSet<Id>, row_id: Id) -> Self {
        Self {
            scrollable_id,
            tracked_row_ids,
            row_id,
            scrollable: None,
            row_bounds: HashMap::new(),
        }
    }
}

impl Operation<MeasuredSelectedResultVisibility> for MeasureSelectedResultVisibilityOperation {
    fn traverse(
        &mut self,
        operate_on_children: &mut dyn FnMut(&mut dyn Operation<MeasuredSelectedResultVisibility>),
    ) {
        operate_on_children(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        let Some(id) = id.cloned() else {
            return;
        };

        if self.tracked_row_ids.contains(&id) {
            self.row_bounds.insert(id, bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scrollable_id) {
            self.scrollable = Some(ScrollableSnapshot {
                bounds,
                content_bounds,
                offset: translation,
            });
        }
    }

    fn finish(&self) -> Outcome<MeasuredSelectedResultVisibility> {
        let Some(scrollable) = self.scrollable else {
            return Outcome::Some(MeasuredSelectedResultVisibility {
                row_bounds: HashMap::new(),
                visible_row_ids: HashSet::new(),
                target_offset_y: None,
            });
        };

        let row_bounds = self
            .row_bounds
            .iter()
            .map(|(id, bounds)| {
                (
                    id.clone(),
                    CachedRowBounds {
                        top: bounds.y - scrollable.content_bounds.y,
                        height: bounds.height,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let target_offset_y = row_bounds.get(&self.row_id).and_then(|row_bounds| {
            scroll_offset_to_reveal_row(
                scrollable.bounds.height,
                scrollable.content_bounds.height,
                scrollable.offset.y,
                row_bounds.top,
                row_bounds.height,
            )
        });
        let visible_row_ids = visible_row_ids_for_viewport(
            scrollable.bounds.height,
            scrollable.offset.y,
            &row_bounds,
        );

        Outcome::Some(MeasuredSelectedResultVisibility {
            row_bounds,
            visible_row_ids,
            target_offset_y,
        })
    }
}

fn scroll_offset_to_reveal_row(
    viewport_height: f32,
    content_height: f32,
    current_offset: f32,
    row_top: f32,
    row_height: f32,
) -> Option<f32> {
    let max_offset = (content_height - viewport_height).max(0.0);
    let visible_top = current_offset;
    let visible_bottom = current_offset + viewport_height;
    let row_bottom = row_top + row_height;

    if row_top < visible_top + RESULTS_SCROLL_MARGIN {
        Some((row_top - RESULTS_SCROLL_MARGIN).max(0.0))
    } else if row_bottom > visible_bottom - RESULTS_SCROLL_MARGIN {
        Some((row_bottom - viewport_height + RESULTS_SCROLL_MARGIN).min(max_offset))
    } else {
        None
    }
}

fn cached_scroll_offset_for_row(viewport: Viewport, row_bounds: CachedRowBounds) -> Option<f32> {
    let viewport_height = viewport.bounds().height;
    let content_height = viewport.content_bounds().height;

    if viewport_height <= 0.0 || content_height <= 0.0 {
        return None;
    }

    // The measured pass is still the source of truth. This uses cached row
    // bounds from the previous layout so the first scroll can move immediately
    // without inventing new geometry.
    scroll_offset_to_reveal_row(
        viewport_height,
        content_height,
        viewport.absolute_offset().y,
        row_bounds.top,
        row_bounds.height,
    )
}

fn visible_row_ids_for_viewport(
    viewport_height: f32,
    current_offset: f32,
    row_bounds: &HashMap<Id, CachedRowBounds>,
) -> HashSet<Id> {
    if viewport_height <= 0.0 {
        return HashSet::new();
    }

    let visible_top = (current_offset - WINDOW_THUMBNAIL_OVERSCAN).max(0.0);
    let visible_bottom = current_offset + viewport_height + WINDOW_THUMBNAIL_OVERSCAN;

    row_bounds
        .iter()
        .filter_map(|(id, bounds)| {
            let row_bottom = bounds.top + bounds.height;
            (row_bottom >= visible_top && bounds.top <= visible_bottom).then(|| id.clone())
        })
        .collect()
}

impl PickerApp {
    fn request_icon_index(&mut self) -> Task<Message> {
        if self.icon_index.is_some() || self.is_loading_icon_index {
            return Task::none();
        }

        self.is_loading_icon_index = true;

        Task::perform(async move { load_icon_index() }, Message::IconIndexFinished)
    }

    fn request_search(&mut self) -> Task<Message> {
        let Some(registry) = self.registry.as_ref().map(Arc::clone) else {
            return Task::none();
        };

        self.search_request_id += 1;
        self.active_search_request_id = self.search_request_id;

        let request_id = self.search_request_id;
        let query = self.query.clone();

        Task::perform(
            async move { search_registry(registry, &query) },
            move |result| Message::SearchFinished { request_id, result },
        )
    }

    fn request_visible_window_thumbnails(&mut self) -> Task<Message> {
        let mut window_ids = Vec::new();

        for result in &self.results {
            if !self.visible_result_row_ids.contains(&row_widget_id(result)) {
                continue;
            }

            let Some(window_id) = window_id_for_result(result) else {
                continue;
            };

            if self.window_thumbnails.contains_key(&window_id) {
                continue;
            }

            self.window_thumbnails
                .insert(window_id, WindowThumbnailState::Loading);
            window_ids.push(window_id);

            if window_ids.len() >= MAX_WINDOW_THUMBNAIL_REQUESTS_PER_BATCH {
                break;
            }
        }

        Task::batch(window_ids.into_iter().map(|window_id| {
            Task::perform(
                async move { fetch_window_thumbnail(window_id) },
                move |result| Message::WindowThumbnailFinished { window_id, result },
            )
        }))
    }

    fn visible_result_row_ids_for_viewport(&self, viewport: Viewport) -> HashSet<Id> {
        visible_row_ids_for_viewport(
            viewport.bounds().height,
            viewport.absolute_offset().y,
            &self.results_row_bounds,
        )
    }

    fn activate_selected(&mut self, action_id: &'static str) -> Task<Message> {
        let Some(result) = self.selected_result().cloned() else {
            return Task::none();
        };
        let Some(registry) = self.registry.as_ref().map(Arc::clone) else {
            return Task::none();
        };

        Task::perform(
            async move { activate_result(registry, result, action_id) },
            Message::ActivationFinished,
        )
    }

    fn selected_result(&self) -> Option<&SearchResult> {
        self.selected_index
            .and_then(|index| self.results.get(index))
    }

    fn handle_search_key(&mut self, key: AppKey) -> Task<Message> {
        match key {
            AppKey::Down | AppKey::Tab if !self.results.is_empty() => {
                self.focus_target = FocusTarget::Results;
                self.selected_index.get_or_insert(0);
                Task::batch([
                    focus_first_result(&self.search_input_id),
                    self.schedule_selected_result_scroll(),
                ])
            }
            AppKey::Escape => iced::exit(),
            _ => Task::none(),
        }
    }

    fn handle_results_key(&mut self, key: AppKey) -> Task<Message> {
        match key {
            AppKey::Down => {
                let Some(index) = self.selected_index else {
                    self.selected_index = (!self.results.is_empty()).then_some(0);
                    return if self.results.is_empty() {
                        Task::none()
                    } else {
                        Task::batch([
                            focus_first_result(&self.search_input_id),
                            self.schedule_selected_result_scroll(),
                        ])
                    };
                };

                if index + 1 < self.results.len() {
                    self.selected_index = Some(index + 1);
                    self.schedule_selected_result_scroll()
                } else {
                    Task::none()
                }
            }
            AppKey::Up => match self.selected_index {
                Some(0) | None => {
                    self.focus_target = FocusTarget::Search;
                    focus(self.search_input_id.clone())
                }
                Some(index) => {
                    self.selected_index = Some(index - 1);
                    self.schedule_selected_result_scroll()
                }
            },
            AppKey::Tab => {
                self.focus_target = FocusTarget::Search;
                focus(self.search_input_id.clone())
            }
            AppKey::FocusSearch => {
                self.focus_target = FocusTarget::Search;
                focus(self.search_input_id.clone())
            }
            AppKey::Enter => self.activate_selected(DEFAULT_ACTION_ID),
            AppKey::Escape => iced::exit(),
            AppKey::Shortcut(shortcut) => self
                .selected_action_id_for_shortcut(shortcut)
                .map_or_else(Task::none, |action_id| {
                    Task::done(Message::ActivateSelectedAction(action_id))
                }),
        }
    }

    fn selected_action_id_for_shortcut(&self, shortcut: char) -> Option<&'static str> {
        let shortcut = shortcut.to_ascii_lowercase();

        self.selected_result()?
            .actions
            .iter()
            .find(|action| action.shortcut.to_ascii_lowercase() == shortcut)
            .map(|action| action.id)
    }

    fn invalidate_selected_result_scroll(&mut self) {
        self.selected_result_visibility_request_id += 1;
    }

    fn schedule_selected_result_scroll(&mut self) -> Task<Message> {
        self.selected_result_visibility_request_id += 1;
        let request_id = self.selected_result_visibility_request_id;

        // We first scroll from cached row geometry right away so the new
        // selection does not disappear for a frame. We still queue a measured
        // follow-up pass because the current selection change may alter row
        // heights, making the cached geometry stale.
        //
        // This needs to be sequential, not batched. If both tasks run in
        // parallel, the corrective measurement can happen before the optimistic
        // scroll has been applied, and the later optimistic scroll can leave
        // the selected row only partially visible.
        self.scroll_selected_result_into_view_from_cache()
            .chain(Task::done(Message::EnsureSelectedResultVisible(request_id)))
    }

    fn scroll_selected_result_into_view_from_cache(&self) -> Task<Message> {
        let Some(result) = self.selected_result() else {
            return Task::none();
        };
        let Some(viewport) = self.results_viewport else {
            return Task::none();
        };
        let row_id = row_widget_id(result);
        let Some(row_bounds) = self.results_row_bounds.get(&row_id).copied() else {
            return Task::none();
        };
        let Some(target_offset) = cached_scroll_offset_for_row(viewport, row_bounds) else {
            return Task::none();
        };

        scroll_to(
            self.results_scroll_id.clone(),
            AbsoluteOffset {
                x: 0.0,
                y: target_offset,
            },
        )
    }

    fn measure_selected_result_visibility(&self, request_id: u64) -> Task<Message> {
        let Some(result) = self.selected_result() else {
            return Task::none();
        };

        let scrollable_id = self.results_scroll_id.clone();
        let row_id = row_widget_id(result);
        let tracked_row_ids = self.results.iter().map(row_widget_id).collect();

        operate(MeasureSelectedResultVisibilityOperation::new(
            scrollable_id.clone(),
            tracked_row_ids,
            row_id,
        ))
        .map(move |measured| Message::SelectedResultVisibilityMeasured {
            request_id,
            measured,
        })
    }
}

fn focus_first_result(search_input_id: &Id) -> Task<Message> {
    Task::batch([focus(search_input_id.clone()), focus_next()])
}

fn search_registry(
    registry: Arc<Mutex<ModuleRegistry>>,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let mut registry = registry
        .lock()
        .map_err(|_| "module registry lock poisoned".to_string())?;
    registry.search(query).map_err(|error| error.to_string())
}

fn activate_result(
    registry: Arc<Mutex<ModuleRegistry>>,
    result: SearchResult,
    action_id: &'static str,
) -> Result<ActivationOutcome, String> {
    let mut registry = registry
        .lock()
        .map_err(|_| "module registry lock poisoned".to_string())?;

    if action_id == DEFAULT_ACTION_ID {
        registry.activate(&result)
    } else {
        registry.activate_action(&result, action_id)
    }
    .map_err(|error| error.to_string())
}

fn window_id_for_result(result: &SearchResult) -> Option<u64> {
    if result.kind != MatchKind::Window {
        return None;
    }

    result
        .item_id
        .split_once(':')
        .map_or(result.item_id.as_str(), |(window_id, _pid)| window_id)
        .parse()
        .ok()
}

fn window_thumbnail_path_for_result(
    result: &SearchResult,
    thumbnails: &HashMap<u64, WindowThumbnailState>,
) -> Option<PathBuf> {
    let window_id = window_id_for_result(result)?;

    match thumbnails.get(&window_id)? {
        WindowThumbnailState::Ready(path) => Some(path.clone()),
        WindowThumbnailState::Loading | WindowThumbnailState::Failed => None,
    }
}

fn fetch_window_thumbnail(window_id: u64) -> Result<LoadedWindowThumbnail, String> {
    let thumbnail = request_niri_window_thumbnail(window_id)?;

    if thumbnail.id != window_id {
        return Err(format!(
            "niri returned thumbnail for window {}, expected {window_id}",
            thumbnail.id
        ));
    }

    let png = BASE64_STANDARD
        .decode(thumbnail.png_base64.as_bytes())
        .map_err(|error| format!("failed to decode thumbnail PNG: {error}"))?;
    let path = write_window_thumbnail(window_id, &png)?;

    Ok(LoadedWindowThumbnail { window_id, path })
}

fn request_niri_window_thumbnail(window_id: u64) -> Result<NiriWindowThumbnail, String> {
    let socket_path =
        env::var_os("NIRI_SOCKET").ok_or_else(|| "NIRI_SOCKET is not set".to_string())?;
    let mut socket = UnixStream::connect(socket_path)
        .map_err(|error| format!("failed to connect to niri: {error}"))?;
    socket
        .set_read_timeout(Some(NIRI_THUMBNAIL_REQUEST_TIMEOUT))
        .map_err(|error| format!("failed to configure niri socket read timeout: {error}"))?;
    socket
        .set_write_timeout(Some(NIRI_THUMBNAIL_REQUEST_TIMEOUT))
        .map_err(|error| format!("failed to configure niri socket write timeout: {error}"))?;
    let request = json!({
        "WindowThumbnail": {
            "id": window_id,
            "max_width": WINDOW_THUMBNAIL_MAX_WIDTH,
            "max_height": WINDOW_THUMBNAIL_MAX_HEIGHT,
        }
    });

    serde_json::to_writer(&mut socket, &request)
        .map_err(|error| format!("failed to encode niri thumbnail request: {error}"))?;
    socket
        .write_all(b"\n")
        .map_err(|error| format!("failed to send niri thumbnail request: {error}"))?;

    let mut reply_line = String::new();
    BufReader::new(socket)
        .read_line(&mut reply_line)
        .map_err(|error| format!("failed to read niri thumbnail reply: {error}"))?;

    let reply = serde_json::from_str::<NiriIpcReply>(&reply_line)
        .map_err(|error| format!("failed to parse niri thumbnail reply: {error}"))?;

    match reply {
        NiriIpcReply::Ok(NiriIpcResponse::WindowThumbnail(thumbnail)) => Ok(thumbnail),
        NiriIpcReply::Err(error) => Err(error),
    }
}

fn write_window_thumbnail(window_id: u64, png: &[u8]) -> Result<PathBuf, String> {
    let dir = env::temp_dir().join(format!("picky-window-thumbnails-{}", process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create thumbnail cache directory: {error}"))?;

    let path = dir.join(format!("window-{window_id}.png"));
    fs::write(&path, png).map_err(|error| format!("failed to write thumbnail PNG: {error}"))?;

    Ok(path)
}

fn leading_visual(
    result: &SearchResult,
    icon_path: Option<PathBuf>,
    window_thumbnail_path: Option<PathBuf>,
    _is_selected: bool,
) -> Element<'static, Message> {
    if result.kind == MatchKind::Window {
        return window_thumbnail_visual(result, icon_path, window_thumbnail_path);
    }

    if result.kind == MatchKind::Application {
        if let Some(icon_path) = icon_path {
            return icon_widget(icon_path, RESULT_ICON_SIZE);
        }
    } else if result.kind == MatchKind::Notification {
        if let Some(icon_path) = icon_path {
            return column![
                text(kind_symbol(result)).size(20),
                icon_widget(icon_path, SUBTITLE_ICON_SIZE),
            ]
            .align_x(Alignment::Center)
            .spacing(4)
            .width(Length::Fixed(RESULT_ICON_SIZE + 8.0))
            .into();
        }
    }

    text(kind_symbol(result))
        .size(24)
        .width(Length::Fixed(RESULT_ICON_SIZE))
        .into()
}

fn window_thumbnail_visual(
    result: &SearchResult,
    icon_path: Option<PathBuf>,
    thumbnail_path: Option<PathBuf>,
) -> Element<'static, Message> {
    let content = if let Some(thumbnail_path) = thumbnail_path {
        image(image::Handle::from_path(thumbnail_path))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else if let Some(icon_path) = icon_path {
        icon_widget(icon_path, RESULT_ICON_SIZE)
    } else {
        text(kind_symbol(result)).size(24).into()
    };

    container(content)
        .center_x(Length::Fixed(WINDOW_THUMBNAIL_WIDTH))
        .center_y(Length::Fixed(WINDOW_THUMBNAIL_HEIGHT))
        .clip(true)
        .style(window_thumbnail_style)
        .into()
}

static ICON_SEARCH_ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
struct IconIndex {
    paths: HashMap<String, PathBuf>,
}

impl IconIndex {
    fn resolve(&self, icon_name: &str) -> Option<PathBuf> {
        let icon_name = icon_name.trim();
        if icon_name.is_empty() {
            return None;
        }

        let file_name = Path::new(icon_name)
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or(icon_name)
            .to_ascii_lowercase();
        let file_stem = Path::new(icon_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(icon_name)
            .to_ascii_lowercase();

        self.paths
            .get(&file_name)
            .or_else(|| self.paths.get(&file_stem))
            .cloned()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum IconFormat {
    Raster,
    Svg,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IconCandidate {
    path: PathBuf,
    score: i32,
}

fn load_icon_index() -> Arc<IconIndex> {
    Arc::new(build_icon_index(
        ICON_SEARCH_ROOTS.get_or_init(build_icon_search_roots),
    ))
}

fn build_icon_index(roots: &[PathBuf]) -> IconIndex {
    let mut candidates = HashMap::new();

    for (root_index, root) in roots.iter().enumerate() {
        index_icon_search_root(root, root_index, &mut candidates);
    }

    IconIndex {
        paths: candidates
            .into_iter()
            .map(|(key, candidate)| (key, candidate.path))
            .collect(),
    }
}

fn index_icon_search_root(
    root: &Path,
    root_index: usize,
    candidates: &mut HashMap<String, IconCandidate>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            index_icon_search_root(&path, root_index, candidates);
            continue;
        }

        let Some(candidate) = icon_candidate(&path, root_index) else {
            continue;
        };

        let file_name = path.file_name().and_then(|file_name| file_name.to_str());
        let file_stem = path.file_stem().and_then(|stem| stem.to_str());

        if let Some(file_name) = file_name {
            index_icon_candidate(
                candidates,
                file_name.to_ascii_lowercase(),
                candidate.clone(),
            );
        }

        if let Some(file_stem) = file_stem {
            index_icon_candidate(candidates, file_stem.to_ascii_lowercase(), candidate);
        }
    }
}

fn index_icon_candidate(
    candidates: &mut HashMap<String, IconCandidate>,
    key: String,
    candidate: IconCandidate,
) {
    let should_replace = candidates
        .get(&key)
        .is_none_or(|current| candidate.score > current.score);

    if should_replace {
        candidates.insert(key, candidate);
    }
}

fn icon_widget(path: PathBuf, size: f32) -> Element<'static, Message> {
    match icon_format(&path) {
        Some(IconFormat::Svg) => svg(path).width(size).height(size).into(),
        Some(IconFormat::Raster) => image(image::Handle::from_path(path))
            .width(size)
            .height(size)
            .into(),
        None => text("?").size(size).into(),
    }
}

fn resolve_icon_path(icon_name: Option<&str>, icon_index: Option<&IconIndex>) -> Option<PathBuf> {
    let icon_name = icon_name
        .map(str::trim)
        .filter(|icon_name| !icon_name.is_empty())?;

    if let Some(path) = explicit_icon_file_path(icon_name) {
        return Some(path);
    }

    icon_index?.resolve(icon_name)
}

fn explicit_icon_file_path(icon_name: &str) -> Option<PathBuf> {
    let path = Path::new(icon_name);

    if path.is_absolute() && path.is_file() && icon_format(path).is_some() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn build_icon_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join(".icons"));
    }

    if let Some(data_home) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        roots.push(data_home.join("icons"));
        roots.push(data_home.join("pixmaps"));
    }

    let data_dirs = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());

    for segment in data_dirs.split(':').filter(|segment| !segment.is_empty()) {
        let root = PathBuf::from(segment);
        roots.push(root.join("icons"));
        roots.push(root.join("pixmaps"));
    }

    let mut seen = HashSet::new();
    roots.retain(|root| root.is_dir() && seen.insert(root.clone()));
    roots
}

fn icon_candidate(path: &Path, root_index: usize) -> Option<IconCandidate> {
    let format = icon_format(path)?;

    Some(IconCandidate {
        path: path.to_path_buf(),
        score: icon_candidate_score(path, format, root_index),
    })
}

fn icon_candidate_score(path: &Path, format: IconFormat, root_index: usize) -> i32 {
    let mut score = match format {
        IconFormat::Svg => 90,
        IconFormat::Raster => 70,
    };

    let root_penalty = i32::try_from(root_index).unwrap_or(i32::MAX).min(20);
    score -= root_penalty;

    for component in path.components() {
        let value = component.as_os_str().to_string_lossy();

        if value.eq_ignore_ascii_case("apps") {
            score += 20;
        } else if value.eq_ignore_ascii_case("scalable") {
            score += 18;
        } else if let Some(size) = icon_directory_size(&value) {
            score += 18 - (size - 32).abs().min(18);
        }
    }

    score
}

fn icon_directory_size(component: &str) -> Option<i32> {
    let (width, height) = component.split_once('x')?;
    let width = width.parse::<i32>().ok()?;
    let height = height
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<i32>()
        .ok()?;

    (width == height).then_some(width)
}

fn icon_format(path: &Path) -> Option<IconFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" => {
            Some(IconFormat::Raster)
        }
        "svg" => Some(IconFormat::Svg),
        _ => None,
    }
}

fn kind_symbol(result: &SearchResult) -> &'static str {
    match result.kind {
        MatchKind::Application => "📦",
        MatchKind::Notification => "🔔",
        MatchKind::Window => "🗖",
        MatchKind::Workspace => "🖥",
    }
}

fn view_action_chip(action: &ResultAction, is_selected: bool) -> Element<'static, Message> {
    let label_color = if is_selected {
        color_from_hex(0xF7FAFF)
    } else {
        theme_text()
    };

    let shortcut_background = if is_selected {
        color_from_rgba_hex(0xFFFFFF, 0.16)
    } else {
        color_from_rgba_hex(0x86A8FF, 0.12)
    };

    container(
        row![
            container(
                text(action.shortcut.to_string())
                    .size(12)
                    .color(if is_selected {
                        label_color
                    } else {
                        theme_blue()
                    }),
            )
            .padding([4, 8])
            .style(move |_| {
                container::Style::default()
                    .background(shortcut_background)
                    .border(border::rounded(CHIP_RADIUS))
            }),
            text(action.label).size(13).color(label_color),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    )
    .padding([7, 10])
    .style(move |_| {
        container::Style::default()
            .background(if is_selected {
                color_from_rgba_hex(0xFFFFFF, 0.10)
            } else {
                color_from_hex(0x212842)
            })
            .border(border::rounded(CHIP_RADIUS).width(1).color(if is_selected {
                color_from_rgba_hex(0xFFFFFF, 0.12)
            } else {
                theme_border()
            }))
    })
    .into()
}

fn view_status_banner(
    label: &'static str,
    message: String,
    tone: BannerTone,
) -> Element<'static, Message> {
    let (accent, background, border_color) = match tone {
        BannerTone::Warning => (
            color_from_hex(0xE7B779),
            color_from_rgba_hex(0xE7B779, 0.10),
            color_from_rgba_hex(0xE7B779, 0.30),
        ),
        BannerTone::Danger => (
            theme_error(),
            color_from_rgba_hex(0xE18497, 0.10),
            color_from_rgba_hex(0xE18497, 0.30),
        ),
    };

    container(
        column![
            text(label.to_uppercase()).size(11).color(accent),
            text(message).size(14).color(theme_text()),
        ]
        .spacing(6),
    )
    .padding(14)
    .style(move |_| {
        container::Style::default()
            .background(background)
            .border(border::rounded(CARD_RADIUS).width(1).color(border_color))
    })
    .into()
}

fn app_background_style(_theme: &Theme) -> container::Style {
    container::Style::default().color(theme_text())
}

fn shell_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(theme_shell_surface())
        .border(
            border::rounded(SHELL_RADIUS)
                .width(2)
                .color(color_from_hex(0x3C4567)),
        )
        .shadow(Shadow {
            color: color_from_rgba_hex(0x050913, 0.42),
            offset: Vector::new(0.0, 14.0),
            blur_radius: 36.0,
        })
        .color(theme_text())
}

fn panel_card_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(theme_panel_surface())
        .border(border::rounded(CARD_RADIUS).width(1).color(theme_border()))
        .color(theme_text())
}

fn window_thumbnail_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(color_from_hex(0x151A2D))
        .border(border::rounded(8).width(1).color(theme_border()))
        .color(theme_text())
}

fn results_surface_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(color_from_hex(0x1F2437))
        .border(border::rounded(CARD_RADIUS).width(1).color(theme_border()))
        .color(theme_text())
}

fn search_input_style(
    _theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let border_color = match status {
        iced::widget::text_input::Status::Active => theme_border(),
        iced::widget::text_input::Status::Hovered => color_from_hex(0x4A567D),
        iced::widget::text_input::Status::Focused { .. } => theme_blue(),
        iced::widget::text_input::Status::Disabled => color_from_hex(0x303750),
    };

    let background = match status {
        iced::widget::text_input::Status::Disabled => color_from_hex(0x20253A),
        iced::widget::text_input::Status::Active
        | iced::widget::text_input::Status::Hovered
        | iced::widget::text_input::Status::Focused { .. } => color_from_hex(0x1A2033),
    };

    iced::widget::text_input::Style {
        background: Background::Color(background),
        border: border::rounded(CARD_RADIUS).width(1).color(border_color),
        icon: theme_secondary_text(),
        placeholder: theme_muted_text(),
        value: theme_text(),
        selection: color_from_rgba_hex(0x86A8FF, 0.28),
    }
}

fn results_scrollable_style(
    _theme: &Theme,
    status: iced::widget::scrollable::Status,
) -> iced::widget::scrollable::Style {
    let is_active = matches!(
        status,
        iced::widget::scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        } | iced::widget::scrollable::Status::Dragged {
            is_vertical_scrollbar_dragged: true,
            ..
        }
    );

    let scroller_color = if is_active {
        color_from_hex(0x6C8EF4)
    } else {
        color_from_hex(0x465176)
    };

    let rail = iced::widget::scrollable::Rail {
        background: Some(Background::Color(color_from_rgba_hex(0x0C111C, 0.18))),
        border: border::rounded(CHIP_RADIUS),
        scroller: iced::widget::scrollable::Scroller {
            background: Background::Color(scroller_color),
            border: border::rounded(CHIP_RADIUS)
                .width(1)
                .color(color_from_rgba_hex(0xA5BEFF, 0.18)),
        },
    };

    iced::widget::scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: iced::widget::scrollable::AutoScroll {
            background: Background::Color(color_from_rgba_hex(0x0C111C, 0.30)),
            border: border::rounded(12),
            shadow: Shadow::default(),
            icon: theme_text(),
        },
    }
}

fn color_from_hex(hex: u32) -> Color {
    let red = ((hex >> 16) & 0xFF) as u8;
    let green = ((hex >> 8) & 0xFF) as u8;
    let blue = (hex & 0xFF) as u8;
    Color::from_rgb8(red, green, blue)
}

fn color_from_rgba_hex(hex: u32, alpha: f32) -> Color {
    let red = ((hex >> 16) & 0xFF) as u8;
    let green = ((hex >> 8) & 0xFF) as u8;
    let blue = (hex & 0xFF) as u8;
    Color::from_rgba8(red, green, blue, alpha)
}

fn theme_shell_surface() -> Color {
    color_from_hex(0x1B2135)
}

fn theme_panel_surface() -> Color {
    color_from_hex(0x272E47)
}

fn theme_card_surface() -> Color {
    color_from_hex(0x252C45)
}

fn theme_border() -> Color {
    color_from_hex(0x3B4465)
}

fn theme_text() -> Color {
    color_from_hex(0xE5EAFE)
}

fn theme_secondary_text() -> Color {
    color_from_hex(0xA2ABC9)
}

fn theme_muted_text() -> Color {
    color_from_hex(0x7B84A7)
}

fn theme_blue() -> Color {
    color_from_hex(0x86A8FF)
}

fn theme_error() -> Color {
    color_from_hex(0xE18497)
}

fn load_startup_context() -> StartupContext {
    StartupContext {
        registry: Arc::new(Mutex::new(ModuleRegistry::new(modules::default_modules()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn app_with_results(results: Vec<SearchResult>) -> PickerApp {
        PickerApp {
            registry: Some(Arc::new(Mutex::new(ModuleRegistry::new(Vec::new())))),
            icon_index: None,
            query: String::new(),
            error_message: String::new(),
            renderer_warning: String::new(),
            selected_index: (!results.is_empty()).then_some(0),
            results,
            focus_target: FocusTarget::Search,
            search_request_id: 0,
            active_search_request_id: 0,
            search_input_id: Id::unique(),
            results_scroll_id: Id::new("results-scroll"),
            results_viewport: None,
            results_row_bounds: HashMap::new(),
            visible_result_row_ids: HashSet::new(),
            window_thumbnails: HashMap::new(),
            selected_result_visibility_request_id: 0,
            is_starting_up: false,
            is_loading_icon_index: false,
        }
    }

    fn result(title: &str, actions: Vec<ResultAction>) -> SearchResult {
        SearchResult {
            module_key: "test",
            item_id: title.to_string(),
            title: title.to_string(),
            subtitle: String::new(),
            icon_name: None,
            kind: MatchKind::Application,
            actions,
            score: 1,
        }
    }

    fn window_result(item_id: &str) -> SearchResult {
        SearchResult {
            module_key: "niri-windows",
            item_id: item_id.to_string(),
            title: format!("Window {item_id}"),
            subtitle: String::new(),
            icon_name: None,
            kind: MatchKind::Window,
            actions: Vec::new(),
            score: 1,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("picky-{name}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn create_icon_file(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, []).unwrap();
    }

    #[test]
    fn query_changed_updates_query_and_requests_search() {
        let mut app = app_with_results(Vec::new());

        let _ = update(&mut app, Message::QueryChanged("fire".to_string()));

        assert_eq!(app.query, "fire");
        assert_eq!(app.focus_target, FocusTarget::Search);
        assert_eq!(app.search_request_id, 1);
        assert_eq!(app.active_search_request_id, 1);
    }

    #[test]
    fn query_changed_while_starting_up_defers_search_until_registry_is_ready() {
        let mut app = app_with_results(Vec::new());
        app.registry = None;
        app.is_starting_up = true;

        let _ = update(&mut app, Message::QueryChanged("fire".to_string()));

        assert_eq!(app.query, "fire");
        assert_eq!(app.search_request_id, 0);
        assert_eq!(app.active_search_request_id, 0);
    }

    #[test]
    fn startup_finished_marks_ready_and_requests_search_for_current_query() {
        let mut app = app_with_results(Vec::new());
        app.registry = None;
        app.is_starting_up = true;
        app.query = "fire".to_string();

        let _ = update(
            &mut app,
            Message::StartupFinished(StartupContext {
                registry: Arc::new(Mutex::new(ModuleRegistry::new(Vec::new()))),
            }),
        );

        assert!(!app.is_starting_up);
        assert!(app.registry.is_some());
        assert_eq!(app.search_request_id, 1);
        assert_eq!(app.active_search_request_id, 1);
    }

    #[test]
    fn down_from_search_moves_focus_to_results() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);

        let _ = app.handle_search_key(AppKey::Down);

        assert_eq!(app.focus_target, FocusTarget::Results);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn tab_from_search_moves_focus_to_results() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);

        let _ = app.handle_search_key(AppKey::Tab);

        assert_eq!(app.focus_target, FocusTarget::Results);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn down_and_up_move_results_selection() {
        let mut app = app_with_results(vec![
            result("Firefox", Vec::new()),
            result("Calculator", Vec::new()),
        ]);
        app.focus_target = FocusTarget::Results;

        let _ = app.handle_results_key(AppKey::Down);
        assert_eq!(app.selected_index, Some(1));

        let _ = app.handle_results_key(AppKey::Up);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn up_at_first_result_returns_focus_to_search() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);
        app.focus_target = FocusTarget::Results;
        app.selected_index = Some(0);

        let _ = app.handle_results_key(AppKey::Up);

        assert_eq!(app.focus_target, FocusTarget::Search);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn slash_from_results_returns_focus_to_search() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);
        app.focus_target = FocusTarget::Results;
        app.selected_index = Some(0);

        let _ = app.handle_results_key(AppKey::FocusSearch);

        assert_eq!(app.focus_target, FocusTarget::Search);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn tab_from_results_returns_focus_to_search() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);
        app.focus_target = FocusTarget::Results;
        app.selected_index = Some(0);

        let _ = app.handle_results_key(AppKey::Tab);

        assert_eq!(app.focus_target, FocusTarget::Search);
        assert_eq!(app.selected_index, Some(0));
    }

    #[test]
    fn shortcut_selects_matching_action_case_insensitively() {
        let app = app_with_results(vec![result(
            "Firefox",
            vec![ResultAction {
                id: "close",
                label: "close",
                shortcut: 'Q',
            }],
        )]);

        assert_eq!(app.selected_action_id_for_shortcut('q'), Some("close"));
        assert_eq!(app.selected_action_id_for_shortcut('Q'), Some("close"));
    }

    #[test]
    fn result_activated_updates_selection_and_focus() {
        let mut app = app_with_results(vec![
            result("Firefox", Vec::new()),
            result("Calculator", Vec::new()),
        ]);
        app.focus_target = FocusTarget::Search;
        app.selected_index = Some(0);

        let _ = update(&mut app, Message::ResultActivated(1));

        assert_eq!(app.selected_index, Some(1));
        assert_eq!(app.focus_target, FocusTarget::Results);
    }

    #[test]
    fn stale_search_results_are_ignored() {
        let mut app = app_with_results(vec![result("Old", Vec::new())]);
        app.active_search_request_id = 2;

        let _ = update(
            &mut app,
            Message::SearchFinished {
                request_id: 1,
                result: Ok(vec![result("New", Vec::new())]),
            },
        );

        assert_eq!(app.results[0].title, "Old");
    }

    #[test]
    fn scroll_offset_moves_up_when_row_is_above_viewport() {
        let offset = scroll_offset_to_reveal_row(100.0, 300.0, 80.0, 40.0, 20.0);

        assert_eq!(offset, Some(32.0));
    }

    #[test]
    fn scroll_offset_moves_down_when_row_is_below_viewport() {
        let offset = scroll_offset_to_reveal_row(100.0, 300.0, 20.0, 110.0, 20.0);

        assert_eq!(offset, Some(38.0));
    }

    #[test]
    fn scroll_offset_stays_put_when_row_is_visible() {
        let offset = scroll_offset_to_reveal_row(100.0, 300.0, 20.0, 40.0, 20.0);

        assert_eq!(offset, None);
    }

    #[test]
    fn window_id_for_result_reads_niri_window_item_ids() {
        assert_eq!(window_id_for_result(&window_result("42")), Some(42));
        assert_eq!(window_id_for_result(&window_result("42:1234")), Some(42));
        assert_eq!(
            window_id_for_result(&window_result("not-a-window-id")),
            None
        );
        assert_eq!(window_id_for_result(&result("42:1234", Vec::new())), None);
    }

    #[test]
    fn visible_row_ids_include_viewport_and_thumbnail_overscan() {
        let above = Id::new("above");
        let in_overscan_above = Id::new("in-overscan-above");
        let in_viewport = Id::new("in-viewport");
        let in_overscan_below = Id::new("in-overscan-below");
        let below = Id::new("below");
        let mut row_bounds = HashMap::new();
        row_bounds.insert(
            above.clone(),
            CachedRowBounds {
                top: 0.0,
                height: 30.0,
            },
        );
        row_bounds.insert(
            in_overscan_above.clone(),
            CachedRowBounds {
                top: 35.0,
                height: 10.0,
            },
        );
        row_bounds.insert(
            in_viewport.clone(),
            CachedRowBounds {
                top: 220.0,
                height: 40.0,
            },
        );
        row_bounds.insert(
            in_overscan_below.clone(),
            CachedRowBounds {
                top: 450.0,
                height: 10.0,
            },
        );
        row_bounds.insert(
            below.clone(),
            CachedRowBounds {
                top: 461.0,
                height: 20.0,
            },
        );

        let visible = visible_row_ids_for_viewport(100.0, 200.0, &row_bounds);

        assert!(!visible.contains(&above));
        assert!(visible.contains(&in_overscan_above));
        assert!(visible.contains(&in_viewport));
        assert!(visible.contains(&in_overscan_below));
        assert!(!visible.contains(&below));
    }

    #[test]
    fn visible_window_thumbnail_requests_are_marked_loading_in_batches() {
        let results = (1..=8)
            .map(|window_id| window_result(&window_id.to_string()))
            .collect::<Vec<_>>();
        let mut app = app_with_results(results);
        app.visible_result_row_ids = app.results.iter().map(row_widget_id).collect();

        let _ = app.request_visible_window_thumbnails();

        let loading_count = app
            .window_thumbnails
            .values()
            .filter(|state| **state == WindowThumbnailState::Loading)
            .count();
        assert_eq!(loading_count, MAX_WINDOW_THUMBNAIL_REQUESTS_PER_BATCH);
        assert_eq!(app.window_thumbnails.len(), 6);
        assert!(!app.window_thumbnails.contains_key(&7));
    }

    #[test]
    fn search_results_replace_state_and_select_first() {
        let mut app = app_with_results(Vec::new());
        app.active_search_request_id = 1;

        let _ = update(
            &mut app,
            Message::SearchFinished {
                request_id: 1,
                result: Ok(vec![
                    result("Firefox", Vec::new()),
                    result("Calc", Vec::new()),
                ]),
            },
        );

        assert_eq!(app.results.len(), 2);
        assert_eq!(app.selected_index, Some(0));
        assert!(app.error_message.is_empty());
    }

    #[test]
    fn search_error_clears_results_and_error_message() {
        let mut app = app_with_results(vec![result("Firefox", Vec::new())]);
        app.active_search_request_id = 1;
        app.focus_target = FocusTarget::Results;

        let _ = update(
            &mut app,
            Message::SearchFinished {
                request_id: 1,
                result: Err("boom".to_string()),
            },
        );

        assert!(app.results.is_empty());
        assert_eq!(app.selected_index, None);
        assert_eq!(app.error_message, "boom");
        assert_eq!(app.focus_target, FocusTarget::Search);
    }

    #[test]
    fn refresh_activation_requests_new_search() {
        let mut app = app_with_results(Vec::new());

        let _ = update(
            &mut app,
            Message::ActivationFinished(Ok(ActivationOutcome::RefreshResults)),
        );

        assert_eq!(app.search_request_id, 1);
        assert_eq!(app.active_search_request_id, 1);
    }

    #[test]
    fn tiny_skia_backend_sets_warning() {
        let mut app = app_with_results(Vec::new());

        let _ = update(
            &mut app,
            Message::SystemInfoLoaded(system::Information {
                system_name: None,
                system_kernel: None,
                system_version: None,
                system_short_version: None,
                cpu_brand: String::new(),
                cpu_cores: None,
                memory_total: 0,
                memory_used: None,
                graphics_backend: "tiny-skia".to_string(),
                graphics_adapter: "software".to_string(),
            }),
        );

        assert_eq!(
            app.renderer_warning,
            "Using software renderer (tiny-skia); performance may be degraded."
        );
    }

    #[test]
    fn non_tiny_skia_backend_clears_warning() {
        let mut app = app_with_results(Vec::new());
        app.renderer_warning = "old warning".to_string();

        let _ = update(
            &mut app,
            Message::SystemInfoLoaded(system::Information {
                system_name: None,
                system_kernel: None,
                system_version: None,
                system_short_version: None,
                cpu_brand: String::new(),
                cpu_cores: None,
                memory_total: 0,
                memory_used: None,
                graphics_backend: "wgpu".to_string(),
                graphics_adapter: "gpu".to_string(),
            }),
        );

        assert!(app.renderer_warning.is_empty());
    }

    #[test]
    fn resolve_icon_path_defers_theme_icons_until_index_is_ready() {
        assert_eq!(resolve_icon_path(Some("firefox"), None), None);
    }

    #[test]
    fn resolve_icon_path_keeps_explicit_icon_files_without_index() {
        let root = temp_dir("explicit-icon");
        let icon_path = root.join("firefox.png");
        create_icon_file(&icon_path);

        let resolved = resolve_icon_path(icon_path.to_str(), None);

        assert_eq!(resolved, Some(icon_path));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn icon_index_resolves_theme_png_icons() {
        let root = temp_dir("png-icon");
        let icon_path = root.join("icons/hicolor/32x32/apps/firefox.png");
        create_icon_file(&icon_path);
        let icon_index = build_icon_index(&[root.join("icons")]);

        let resolved = icon_index.resolve("firefox");

        assert_eq!(resolved, Some(icon_path));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn icon_index_resolves_pixmaps_icons() {
        let root = temp_dir("pixmaps-icon");
        let icon_path = root.join("pixmaps/org.gnome.Calculator.png");
        create_icon_file(&icon_path);
        let icon_index = build_icon_index(&[root.join("pixmaps")]);

        let resolved = icon_index.resolve("org.gnome.Calculator");

        assert_eq!(resolved, Some(icon_path));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn icon_index_prefers_scalable_svg_icons() {
        let root = temp_dir("svg-icon");
        let png_path = root.join("icons/hicolor/32x32/apps/firefox.png");
        let svg_path = root.join("icons/hicolor/scalable/apps/firefox.svg");
        create_icon_file(&png_path);
        create_icon_file(&svg_path);
        let icon_index = build_icon_index(&[root.join("icons")]);

        let resolved = icon_index.resolve("firefox");

        assert_eq!(resolved, Some(svg_path));
        let _ = fs::remove_dir_all(root);
    }
}
