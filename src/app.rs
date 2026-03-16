use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use iced::event;
use iced::keyboard::{self, Key, key::Named};
use iced::system;
use iced::widget::{
    scrollable, Id, button, column, container, image, keyed_column, lazy, mouse_area, row, text,
    text_input,
};
use iced::widget::operation::{focus, focus_next};
use iced::{Background, Element, Length, Size, Subscription, Task, Theme, border, window};
use serde::Deserialize;

use crate::module::{
    ActivationOutcome, MatchKind, ModuleRegistry, ResultAction, SearchResult, DEFAULT_ACTION_ID,
};
use crate::modules;

const WINDOW_WIDTH: f32 = 820.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 680.0;
const WINDOW_HEIGHT_FRACTION: f32 = 0.7;
const RESULT_ICON_SIZE: f32 = 28.0;
const SUBTITLE_ICON_SIZE: f32 = 20.0;

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
    KeyPressed(AppKey),
    SystemInfoLoaded(system::Information),
    SearchFinished {
        request_id: u64,
        result: Result<Vec<SearchResult>, String>,
    },
    ActivationFinished(Result<ActivationOutcome, String>),
}

pub struct PickerApp {
    registry: Arc<Mutex<ModuleRegistry>>,
    query: String,
    error_message: String,
    renderer_warning: String,
    results: Vec<SearchResult>,
    selected_index: Option<usize>,
    focus_target: FocusTarget,
    search_request_id: u64,
    active_search_request_id: u64,
    search_input_id: Id,
}

#[derive(Debug, Deserialize)]
struct NiriOutput {
    logical: NiriLogicalOutput,
}

#[derive(Debug, Deserialize)]
struct NiriLogicalOutput {
    height: i32,
}

#[derive(Debug, Deserialize)]
struct NiriWorkspaceInfo {
    output: String,
    #[serde(default)]
    is_focused: bool,
}

pub fn run() -> iced::Result {
    iced::application(initialize, update, view)
        .subscription(subscription)
        .theme(theme)
        .window(window::Settings {
            size: Size::new(WINDOW_WIDTH, initial_window_height()),
            position: window::Position::Centered,
            decorations: false,
            resizable: false,
            ..window::Settings::default()
        })
        .run()
}

fn theme(_app: &PickerApp) -> Theme {
    Theme::Dark
}

fn initialize() -> (PickerApp, Task<Message>) {
    let search_input_id = Id::unique();
    let mut app = PickerApp {
        registry: Arc::new(Mutex::new(ModuleRegistry::new(modules::default_modules()))),
        query: String::new(),
        error_message: String::new(),
        renderer_warning: String::new(),
        results: Vec::new(),
        selected_index: None,
        focus_target: FocusTarget::Search,
        search_request_id: 0,
        active_search_request_id: 0,
        search_input_id,
    };

    let task = Task::batch([
        focus(app.search_input_id.clone()),
        system::information().map(Message::SystemInfoLoaded),
        app.request_search(),
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
            app.request_search()
        }
        Message::ActivateSelected => app.activate_selected(DEFAULT_ACTION_ID),
        Message::ActivateSelectedAction(action_id) => app.activate_selected(action_id),
        Message::ResultSelected(index) => {
            app.selected_index = Some(index);
            app.focus_target = FocusTarget::Results;
            Task::none()
        }
        Message::ResultActivated(index) => {
            app.selected_index = Some(index);
            app.focus_target = FocusTarget::Results;
            app.activate_selected(DEFAULT_ACTION_ID)
        }
        Message::SystemInfoLoaded(info) => {
            app.renderer_warning =
                tiny_skia_warning(&info).map_or_else(String::new, str::to_string);
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
                    app.selected_index = (!app.results.is_empty()).then_some(0);

                    if app.focus_target == FocusTarget::Results && !app.results.is_empty() {
                        focus_first_result(&app.search_input_id)
                    } else if app.results.is_empty() && app.focus_target == FocusTarget::Results {
                        app.focus_target = FocusTarget::Search;
                        focus(app.search_input_id.clone())
                    } else {
                        Task::none()
                    }
                }
                Err(error_message) => {
                    app.error_message = error_message;
                    app.results.clear();
                    app.selected_index = None;

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
        .padding(12)
        .size(24);

    let results_list: Element<'_, Message> = if app.results.is_empty() {
        column![text("No matches.")]
            .spacing(8)
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

                (
                    row_key(&row.result),
                    lazy(row, view_result_row).into(),
                )
            })
            .collect::<Vec<_>>();

        keyed_column(rows).spacing(6).width(Length::Fill).into()
    };

    let error = if app.error_message.is_empty() {
        text("")
    } else {
        text(&app.error_message)
    };

    let warning = if app.renderer_warning.is_empty() {
        text("")
    } else {
        text(&app.renderer_warning)
    };

    container(
        column![
            search,
            text(format!("{} results", app.results.len())).size(14),
            warning.size(14),
            scrollable(results_list).height(Length::Fill),
            error.size(14),
        ]
        .spacing(10)
        .padding(18)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ResultRowView {
    index: usize,
    result: SearchResult,
    is_selected: bool,
    show_action_hints: bool,
}

fn view_result_row(row_state: &ResultRowView) -> Element<'static, Message> {
    let index = row_state.index;
    let is_selected = row_state.is_selected;
    let show_action_hints = row_state.show_action_hints;
    let result = &row_state.result;

    let mut text_column = column![text(result.title.clone()).size(18)].spacing(4);

    if !result.subtitle.trim().is_empty() {
        let subtitle_line = if let Some(icon_path) = subtitle_icon_path(result) {
            row![
                image(image::Handle::from_path(icon_path))
                    .width(SUBTITLE_ICON_SIZE)
                    .height(SUBTITLE_ICON_SIZE),
                text(result.subtitle.clone()).size(14)
            ]
            .spacing(6)
        } else {
            row![text(result.subtitle.clone()).size(14)]
        };

        text_column = text_column.push(subtitle_line);
    }

    if show_action_hints && !result.actions.is_empty() {
        text_column = text_column.push(text(format_action_hints(&result.actions)).size(13));
    }

    let row_content = row![leading_visual(result), text_column.width(Length::Fill)]
        .spacing(10)
        .width(Length::Fill);

    mouse_area(
        button(container(row_content).width(Length::Fill))
            .width(Length::Fill)
            .padding(10)
            .style(move |theme, status| result_row_button_style(theme, status, is_selected))
            .on_press(Message::ResultSelected(index)),
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

fn result_row_button_style(
    theme: &Theme,
    status: button::Status,
    is_selected: bool,
) -> button::Style {
    let palette = theme.extended_palette();

    if is_selected {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => palette.primary.strong.color,
            button::Status::Active | button::Status::Disabled => palette.primary.base.color,
        };

        return button::Style {
            background: Some(Background::Color(background)),
            text_color: palette.primary.base.text,
            border: border::rounded(10)
                .width(1)
                .color(palette.primary.strong.color),
            ..button::Style::default()
        };
    }

    let background = match status {
        button::Status::Hovered => Some(Background::Color(palette.background.weak.color)),
        button::Status::Pressed => Some(Background::Color(palette.background.strong.color)),
        button::Status::Active | button::Status::Disabled => None,
    };

    button::Style {
        background,
        text_color: palette.background.base.text,
        border: border::rounded(10),
        ..button::Style::default()
    }
}

fn tiny_skia_warning(info: &system::Information) -> Option<&'static str> {
    info.graphics_backend
        .eq_ignore_ascii_case("tiny-skia")
        .then_some("Using software renderer (tiny-skia); performance may be degraded.")
}

impl PickerApp {
    fn request_search(&mut self) -> Task<Message> {
        self.search_request_id += 1;
        self.active_search_request_id = self.search_request_id;

        let request_id = self.search_request_id;
        let query = self.query.clone();
        let registry = Arc::clone(&self.registry);

        Task::perform(
            async move { search_registry(registry, &query) },
            move |result| Message::SearchFinished { request_id, result },
        )
    }

    fn activate_selected(&mut self, action_id: &'static str) -> Task<Message> {
        let Some(result) = self.selected_result().cloned() else {
            return Task::none();
        };

        let registry = Arc::clone(&self.registry);
        Task::perform(
            async move { activate_result(registry, result, action_id) },
            Message::ActivationFinished,
        )
    }

    fn selected_result(&self) -> Option<&SearchResult> {
        self.selected_index.and_then(|index| self.results.get(index))
    }

    fn handle_search_key(&mut self, key: AppKey) -> Task<Message> {
        match key {
            AppKey::Down if !self.results.is_empty() => {
                self.focus_target = FocusTarget::Results;
                self.selected_index = Some(0);
                focus_first_result(&self.search_input_id)
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
                        focus_first_result(&self.search_input_id)
                    };
                };

                if index + 1 < self.results.len() {
                    self.selected_index = Some(index + 1);
                    Task::none()
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
                    Task::none()
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
}

fn focus_first_result(search_input_id: &Id) -> Task<Message> {
    Task::batch([
        focus(search_input_id.clone()),
        focus_next(),
    ])
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

fn leading_visual(result: &SearchResult) -> Element<'static, Message> {
    if let Some(icon_path) = leading_icon_path(result) {
        image(image::Handle::from_path(icon_path))
            .width(RESULT_ICON_SIZE)
            .height(RESULT_ICON_SIZE)
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

fn format_action_hints(actions: &[ResultAction]) -> String {
    actions
        .iter()
        .map(|action| format!("{} - {}", action.shortcut, action.label))
        .collect::<Vec<_>>()
        .join("   ")
}

fn initial_window_height() -> f32 {
    window_height_for_output_height(focused_output_height())
}

fn focused_output_height() -> Option<i32> {
    let focused_output = load_focused_output_name()?;
    let outputs = load_outputs().ok()?;

    focused_output_height_for(&focused_output, &outputs)
}

fn focused_output_height_for(
    focused_output: &str,
    outputs: &HashMap<String, NiriOutput>,
) -> Option<i32> {
    outputs
        .get(focused_output)
        .map(|output| output.logical.height)
        .or_else(|| outputs.values().next().map(|output| output.logical.height))
}

fn window_height_for_output_height(output_height: Option<i32>) -> f32 {
    output_height
        .map(|height| ((height as f32) * WINDOW_HEIGHT_FRACTION).round().max(360.0))
        .unwrap_or(DEFAULT_WINDOW_HEIGHT)
}

fn load_focused_output_name() -> Option<String> {
    let workspaces = Command::new("niri")
        .args(["msg", "--json", "workspaces"])
        .output()
        .ok()?;

    if !workspaces.status.success() {
        return None;
    }

    let workspaces: Vec<NiriWorkspaceInfo> = serde_json::from_slice(&workspaces.stdout).ok()?;
    workspaces
        .into_iter()
        .find(|workspace| workspace.is_focused)
        .map(|workspace| workspace.output)
}

fn load_outputs() -> Result<HashMap<String, NiriOutput>, serde_json::Error> {
    let outputs = Command::new("niri")
        .args(["msg", "--json", "outputs"])
        .output()
        .map_err(serde_json::Error::io)?;

    if !outputs.status.success() {
        return Ok(HashMap::new());
    }

    serde_json::from_slice(&outputs.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_results(results: Vec<SearchResult>) -> PickerApp {
        PickerApp {
            registry: Arc::new(Mutex::new(ModuleRegistry::new(Vec::new()))),
            query: String::new(),
            error_message: String::new(),
            renderer_warning: String::new(),
            selected_index: (!results.is_empty()).then_some(0),
            results,
            focus_target: FocusTarget::Search,
            search_request_id: 0,
            active_search_request_id: 0,
            search_input_id: Id::unique(),
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
    fn search_results_replace_state_and_select_first() {
        let mut app = app_with_results(Vec::new());
        app.active_search_request_id = 1;

        let _ = update(
            &mut app,
            Message::SearchFinished {
                request_id: 1,
                result: Ok(vec![result("Firefox", Vec::new()), result("Calc", Vec::new())]),
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
    fn focused_output_height_prefers_focused_output() {
        let outputs = HashMap::from([
            (
                "DP-1".to_string(),
                NiriOutput {
                    logical: NiriLogicalOutput { height: 1000 },
                },
            ),
            (
                "DP-2".to_string(),
                NiriOutput {
                    logical: NiriLogicalOutput { height: 2000 },
                },
            ),
        ]);

        assert_eq!(focused_output_height_for("DP-2", &outputs), Some(2000));
    }

    #[test]
    fn window_height_uses_fraction_and_clamp() {
        assert_eq!(window_height_for_output_height(Some(1000)), 700.0);
        assert_eq!(window_height_for_output_height(Some(100)), 360.0);
        assert_eq!(window_height_for_output_height(None), DEFAULT_WINDOW_HEIGHT);
    }
}
