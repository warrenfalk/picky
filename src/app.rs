use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, ModuleRegistry, ResultAction, SearchResult,
};
use crate::modules;
use iced::advanced::widget::{Operation, operate, operation::Outcome};
use iced::event;
use iced::keyboard::{self, Key, key::Named};
use iced::system;
use iced::widget::operation::{AbsoluteOffset, focus, focus_next, scroll_to};
use iced::widget::scrollable::Viewport;
use iced::widget::{
    Id, button, column, container, image, keyed_column, lazy, mouse_area, row, scrollable, text,
    text_input,
};
use iced::{
    Alignment, Background, Color, Element, Length, Rectangle, Shadow, Size, Subscription, Task,
    Theme, Vector, border, theme, window,
};

const WINDOW_WIDTH: f32 = 820.0;
// We intentionally start taller and avoid runtime `window::resize(...)`.
// Resizing after the window opens appears to change the surface size without
// recomputing layout, which is believed to be an Iced 0.14.0 bug.
const DEFAULT_WINDOW_HEIGHT: f32 = 1368.0;
const RESULT_ICON_SIZE: f32 = 28.0;
const SUBTITLE_ICON_SIZE: f32 = 20.0;
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
    Enter,
    Escape,
    Shortcut(char),
}

#[derive(Clone, Debug)]
enum Message {
    QueryChanged(String),
    ActivateSelected,
    ActivateSelectedAction(&'static str),
    ResultSelected(usize),
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
    SearchFinished {
        request_id: u64,
        result: Result<Vec<SearchResult>, String>,
    },
    ActivationFinished(Result<ActivationOutcome, String>),
}

pub struct PickerApp {
    registry: Option<Arc<Mutex<ModuleRegistry>>>,
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
    selected_result_visibility_request_id: u64,
    is_starting_up: bool,
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
        selected_result_visibility_request_id: 0,
        is_starting_up: true,
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
        Key::Named(Named::Escape) => Some(Message::KeyPressed(AppKey::Escape)),
        _ => None,
    }
}

fn results_key_message(key: Key, _modifiers: keyboard::Modifiers) -> Option<Message> {
    match key {
        Key::Named(Named::ArrowDown) => Some(Message::KeyPressed(AppKey::Down)),
        Key::Named(Named::ArrowUp) => Some(Message::KeyPressed(AppKey::Up)),
        Key::Named(Named::Enter) => Some(Message::KeyPressed(AppKey::Enter)),
        Key::Named(Named::Escape) => Some(Message::KeyPressed(AppKey::Escape)),
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
        Message::ResultSelected(index) => {
            app.selected_index = Some(index);
            app.focus_target = FocusTarget::Results;
            app.schedule_selected_result_scroll()
        }
        Message::ResultActivated(index) => {
            app.selected_index = Some(index);
            app.focus_target = FocusTarget::Results;
            app.activate_selected(DEFAULT_ACTION_ID)
        }
        Message::ResultsScrolled(viewport) => {
            app.results_viewport = Some(viewport);
            Task::none()
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

            measured
                .target_offset_y
                .map_or_else(Task::none, |offset_y| {
                    scroll_to(
                        app.results_scroll_id.clone(),
                        AbsoluteOffset {
                            x: 0.0,
                            y: offset_y,
                        },
                    )
                })
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
                    app.selected_index = (!app.results.is_empty()).then_some(0);

                    if app.focus_target == FocusTarget::Results && !app.results.is_empty() {
                        Task::batch([
                            focus_first_result(&app.search_input_id),
                            app.schedule_selected_result_scroll(),
                        ])
                    } else if !app.results.is_empty() {
                        app.schedule_selected_result_scroll()
                    } else if app.results.is_empty() && app.focus_target == FocusTarget::Results {
                        app.focus_target = FocusTarget::Search;
                        app.invalidate_selected_result_scroll();
                        focus(app.search_input_id.clone())
                    } else {
                        app.invalidate_selected_result_scroll();
                        Task::none()
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
        let subtitle_line = if let Some(icon_path) = subtitle_icon_path(result) {
            row![
                image(image::Handle::from_path(icon_path))
                    .width(SUBTITLE_ICON_SIZE)
                    .height(SUBTITLE_ICON_SIZE),
                text(result.subtitle.clone()).size(14).color(subtitle_color)
            ]
            .align_y(Alignment::Center)
            .spacing(6)
        } else {
            row![text(result.subtitle.clone()).size(14).color(subtitle_color)]
        };

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
        leading_visual(result, is_selected),
        text_column.width(Length::Fill)
    ]
    .align_y(Alignment::Center)
    .spacing(12)
    .width(Length::Fill);

    mouse_area(
        container(
            button(container(row_content).width(Length::Fill))
                .width(Length::Fill)
                .padding(12)
                .style(move |theme, status| result_row_button_style(theme, status, is_selected))
                .on_press(Message::ResultSelected(index)),
        )
        .id(row_id)
        .width(Length::Fill),
    )
    .on_double_click(Message::ResultActivated(index))
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

        Outcome::Some(MeasuredSelectedResultVisibility {
            row_bounds,
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

impl PickerApp {
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
            AppKey::Down if !self.results.is_empty() => {
                self.focus_target = FocusTarget::Results;
                self.selected_index = Some(0);
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

fn leading_visual(result: &SearchResult, _is_selected: bool) -> Element<'static, Message> {
    if let Some(icon_path) = leading_icon_path(result) {
        image(image::Handle::from_path(icon_path))
            .width(RESULT_ICON_SIZE)
            .height(RESULT_ICON_SIZE)
            .into()
    } else if let Some(icon_path) = notification_icon_path(result) {
        column![
            text(kind_symbol(result)).size(20),
            image(image::Handle::from_path(icon_path))
                .width(SUBTITLE_ICON_SIZE)
                .height(SUBTITLE_ICON_SIZE),
        ]
        .align_x(Alignment::Center)
        .spacing(4)
        .width(Length::Fixed(RESULT_ICON_SIZE + 8.0))
        .into()
    } else {
        text(kind_symbol(result))
            .size(24)
            .width(Length::Fixed(RESULT_ICON_SIZE))
            .into()
    }
}

fn leading_icon_path(result: &SearchResult) -> Option<PathBuf> {
    match result.kind {
        MatchKind::Application => icon_file_path(result.icon_name.as_deref()),
        MatchKind::Notification | MatchKind::Window | MatchKind::Workspace => None,
    }
}

fn subtitle_icon_path(result: &SearchResult) -> Option<PathBuf> {
    if result.kind == MatchKind::Window {
        icon_file_path(result.icon_name.as_deref())
    } else {
        None
    }
}

fn notification_icon_path(result: &SearchResult) -> Option<PathBuf> {
    if result.kind == MatchKind::Notification {
        icon_file_path(result.icon_name.as_deref())
    } else {
        None
    }
}

fn icon_file_path(icon_name: Option<&str>) -> Option<PathBuf> {
    let icon_name = icon_name
        .map(str::trim)
        .filter(|icon_name| !icon_name.is_empty())?;
    let path = Path::new(icon_name);

    if path.is_absolute() && path.is_file() {
        Some(path.to_path_buf())
    } else {
        None
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

    fn app_with_results(results: Vec<SearchResult>) -> PickerApp {
        PickerApp {
            registry: Some(Arc::new(Mutex::new(ModuleRegistry::new(Vec::new())))),
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
            selected_result_visibility_request_id: 0,
            is_starting_up: false,
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
}
