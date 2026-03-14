use std::convert::identity;
use std::path::Path;

use relm4::factory::{FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::prelude::*;
use relm4::{Worker, WorkerController};

use gtk::gdk;
use gtk::glib;
use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, Image, Label, ListBox,
    Orientation, PolicyType, ScrolledWindow, SelectionMode, Stack,
};

use crate::module::{
    ActivationOutcome, MatchKind, ModuleRegistry, ResultAction, SearchResult, DEFAULT_ACTION_ID,
};
use crate::modules;

const WINDOW_WIDTH: i32 = 820;
const WINDOW_HEIGHT_FRACTION: f64 = 0.7;
const WINDOW_CONTENT_WIDTH: i32 = WINDOW_WIDTH - 36;
const RESULT_ICON_SIZE: i32 = 28;
const SUBTITLE_ICON_SIZE: i32 = 22;
const RESULTS_STACK_RESULTS: &str = "results";
const RESULTS_STACK_EMPTY: &str = "empty";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusTarget {
    Search,
    Results,
}

#[derive(Debug)]
pub enum AppMsg {
    QueryChanged(String),
    SearchEntryKeyPressed(gdk::Key),
    ResultsKeyPressed(gdk::Key),
    ResultsRowActivated(i32),
    RowSelected(Option<i32>),
    ActivateSelected,
    SearchFinished {
        query: String,
        result: Result<Vec<SearchResult>, String>,
    },
    ActivationFinished(Result<ActivationOutcome, String>),
    Close,
}

#[derive(Debug)]
enum SearchWorkerInput {
    Search(String),
    Activate {
        result: SearchResult,
        action_id: &'static str,
    },
}

#[derive(Debug)]
struct ResultRow {
    result: SearchResult,
    show_action_hints: bool,
}

#[relm4::factory]
impl FactoryComponent for ResultRow {
    type Init = SearchResult;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        root = gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 10,
            set_margin_top: 8,
            set_margin_bottom: 8,
            set_margin_start: 8,
            set_margin_end: 8,
            set_hexpand: true,

            #[name(leading_icon)]
            gtk::Image {
                set_visible: false,
                set_pixel_size: RESULT_ICON_SIZE,
                set_valign: gtk::Align::Start,
            },

            #[name(leading_symbol)]
            gtk::Label {
                set_visible: false,
                set_valign: gtk::Align::Start,
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 4,
                set_hexpand: true,

                #[name(title_label)]
                gtk::Label {
                    set_halign: gtk::Align::Start,
                    set_xalign: 0.0,
                    set_hexpand: true,
                    set_ellipsize: EllipsizeMode::End,
                    set_single_line_mode: true,
                },

                #[name(subtitle_row)]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                    set_hexpand: true,

                    #[name(subtitle_icon)]
                    gtk::Image {
                        set_visible: false,
                        set_pixel_size: SUBTITLE_ICON_SIZE,
                        set_valign: gtk::Align::Start,
                    },

                    #[name(subtitle_label)]
                    gtk::Label {
                        set_halign: gtk::Align::Start,
                        set_xalign: 0.0,
                        set_hexpand: true,
                        set_ellipsize: EllipsizeMode::End,
                        set_single_line_mode: true,
                        add_css_class: "dim-label",
                    }
                },

                #[name(action_label)]
                gtk::Label {
                    set_halign: gtk::Align::Start,
                    set_xalign: 0.0,
                    set_hexpand: true,
                    set_wrap: false,
                    set_ellipsize: EllipsizeMode::End,
                    set_single_line_mode: true,
                    add_css_class: "dim-label",
                    set_visible: false,
                }
            }
        }
    }

    fn init_model(
        result: Self::Init,
        _index: &relm4::factory::DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        Self {
            result,
            show_action_hints: false,
        }
    }

    fn pre_view() {
        widgets.title_label.set_label(&row_title(&self.result));
        widgets.subtitle_label.set_label(&self.result.subtitle);
        widgets
            .subtitle_row
            .set_visible(!self.result.subtitle.trim().is_empty());
        widgets
            .action_label
            .set_label(&format_action_hints(&self.result.actions));
        widgets
            .action_label
            .set_visible(self.show_action_hints && !self.result.actions.is_empty());

        configure_leading_widgets(&self.result, &widgets.leading_icon, &widgets.leading_symbol);
        configure_subtitle_icon(&self.result, &widgets.subtitle_icon);
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

pub struct PickerApp {
    worker: WorkerController<SearchWorker>,
    results: FactoryVecDeque<ResultRow>,
    query: String,
    error_message: String,
    selected_index: Option<usize>,
    focus_target: FocusTarget,
}

pub struct PickerWidgets {
    search_entry: Entry,
    results_label: Label,
    results_stack: Stack,
    list_box: ListBox,
    error_label: Label,
}

impl PickerApp {
    fn request_search(&self) {
        self.worker
            .sender()
            .send(SearchWorkerInput::Search(self.query.clone()))
            .unwrap();
    }

    fn apply_search_results(&mut self, result: Result<Vec<SearchResult>, String>) {
        match result {
            Ok(results) => {
                self.error_message.clear();
                {
                    let mut rows = self.results.guard();
                    rows.clear();
                    for result in results {
                        rows.push_back(result);
                    }
                }
                self.selected_index = (!self.results.is_empty()).then_some(0);
                self.sync_row_visibility();
            }
            Err(err) => {
                self.error_message = err;
                self.results.guard().clear();
                self.selected_index = None;
            }
        }
    }

    fn sync_row_visibility(&mut self) {
        let selected_index = self.selected_index;
        let show_selected = self.focus_target == FocusTarget::Results;
        let mut rows = self.results.guard();

        for (index, row) in rows.iter_mut().enumerate() {
            row.show_action_hints = show_selected && selected_index == Some(index);
        }
    }

    fn move_selection(&mut self, offset: i32) {
        if self.results.is_empty() {
            self.selected_index = None;
            return;
        }

        let current_index = self.selected_index.unwrap_or(0) as i32;
        let next_index = (current_index + offset).clamp(0, self.results.len() as i32 - 1);
        self.selected_index = Some(next_index as usize);
        self.sync_row_visibility();
    }

    fn selected_result(&self) -> Option<&SearchResult> {
        self.selected_index
            .and_then(|index| self.results.get(index))
            .map(|row| &row.result)
    }

    fn selected_action_id_for_shortcut(&self, key: gdk::Key) -> Option<&'static str> {
        let shortcut = key.to_unicode()?.to_ascii_lowercase();

        self.selected_result()?
            .actions
            .iter()
            .find(|action| action.shortcut.to_ascii_lowercase() == shortcut)
            .map(|action| action.id)
    }

    fn activate_selected(&mut self, action_id: &'static str) {
        let Some(result) = self.selected_result().cloned() else {
            return;
        };

        self.worker
            .sender()
            .send(SearchWorkerInput::Activate { result, action_id })
            .unwrap();
    }
}

struct SearchWorker {
    registry: ModuleRegistry,
}

impl Worker for SearchWorker {
    type Init = ();
    type Input = SearchWorkerInput;
    type Output = AppMsg;

    fn init(_init: Self::Init, _sender: relm4::ComponentSender<Self>) -> Self {
        Self {
            registry: ModuleRegistry::new(modules::default_modules()),
        }
    }

    fn update(&mut self, message: Self::Input, sender: relm4::ComponentSender<Self>) {
        match message {
            SearchWorkerInput::Search(query) => {
                let result = self.registry.search(&query).map_err(|err| err.to_string());
                sender
                    .output(AppMsg::SearchFinished { query, result })
                    .unwrap();
            }
            SearchWorkerInput::Activate { result, action_id } => {
                let outcome = if action_id == DEFAULT_ACTION_ID {
                    self.registry.activate(&result)
                } else {
                    self.registry.activate_action(&result, action_id)
                }
                .map_err(|err| err.to_string());

                sender.output(AppMsg::ActivationFinished(outcome)).unwrap();
            }
        }
    }
}

impl Component for PickerApp {
    type CommandOutput = ();
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type Root = ApplicationWindow;
    type Widgets = PickerWidgets;

    fn init_root() -> Self::Root {
        ApplicationWindow::builder()
            .title("picky")
            .default_width(WINDOW_WIDTH)
            .default_height(680)
            .decorated(false)
            .resizable(false)
            .build()
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: relm4::ComponentSender<Self>,
    ) -> relm4::ComponentParts<Self> {
        let container = GtkBox::new(Orientation::Vertical, 10);
        container.set_margin_top(18);
        container.set_margin_bottom(18);
        container.set_margin_start(18);
        container.set_margin_end(18);

        let search_entry = Entry::new();
        search_entry.set_hexpand(true);
        search_entry.set_placeholder_text(Some("Type to search"));

        let results_label = Label::new(None);
        results_label.set_halign(gtk::Align::Start);

        let results: FactoryVecDeque<ResultRow> =
            FactoryVecDeque::builder().launch_default().detach();
        let list_box = results.widget().clone();
        list_box.set_selection_mode(SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_hexpand(true);
        list_box.set_focusable(true);

        let empty_label = Label::new(Some("No matches."));
        empty_label.set_halign(gtk::Align::Start);
        empty_label.set_xalign(0.0);
        empty_label.set_margin_top(8);
        empty_label.set_margin_bottom(8);
        empty_label.set_margin_start(8);
        empty_label.set_margin_end(8);

        let results_stack = Stack::new();
        results_stack.add_named(&list_box, Some(RESULTS_STACK_RESULTS));
        results_stack.add_named(&empty_label, Some(RESULTS_STACK_EMPTY));

        let scroller = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vexpand(true)
            .child(&results_stack)
            .build();
        scroller.set_min_content_width(WINDOW_CONTENT_WIDTH);
        scroller.set_max_content_width(WINDOW_CONTENT_WIDTH);
        scroller.set_propagate_natural_width(false);

        let error_label = Label::new(None);
        error_label.set_halign(gtk::Align::Start);
        error_label.add_css_class("error");

        container.append(&search_entry);
        container.append(&results_label);
        container.append(&scroller);
        container.append(&error_label);
        root.set_child(Some(&container));

        let worker = SearchWorker::builder()
            .detach_worker(())
            .forward(sender.input_sender(), identity);

        let model = PickerApp {
            worker,
            results,
            query: String::new(),
            error_message: String::new(),
            selected_index: None,
            focus_target: FocusTarget::Search,
        };

        {
            let input = sender.input_sender().clone();
            search_entry.connect_changed(move |entry| {
                let _ = input.send(AppMsg::QueryChanged(entry.text().to_string()));
            });
        }

        {
            let input = sender.input_sender().clone();
            search_entry.connect_activate(move |_| {
                let _ = input.send(AppMsg::ActivateSelected);
            });
        }

        {
            let input = sender.input_sender().clone();
            let key_controller = EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Down {
                    let _ = input.send(AppMsg::SearchEntryKeyPressed(key));
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            search_entry.add_controller(key_controller);
        }

        {
            let input = sender.input_sender().clone();
            list_box.connect_row_activated(move |_, row: &gtk::ListBoxRow| {
                let _ = input.send(AppMsg::ResultsRowActivated(row.index()));
            });
        }

        {
            let input = sender.input_sender().clone();
            list_box.connect_row_selected(move |_, row: Option<&gtk::ListBoxRow>| {
                let _ = input.send(AppMsg::RowSelected(row.map(|row| row.index())));
            });
        }

        {
            let input = sender.input_sender().clone();
            let key_controller = EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                if matches!(
                    key,
                    gdk::Key::Down | gdk::Key::Up | gdk::Key::Return | gdk::Key::KP_Enter
                ) || key.to_unicode().is_some()
                {
                    let _ = input.send(AppMsg::ResultsKeyPressed(key));
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            list_box.add_controller(key_controller);
        }

        {
            let input = sender.input_sender().clone();
            let key_controller = EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    let _ = input.send(AppMsg::Close);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            root.add_controller(key_controller);
        }

        let widgets = PickerWidgets {
            search_entry,
            results_label,
            results_stack,
            list_box,
            error_label,
        };

        sync_view(&model, &widgets);
        model.request_search();

        root.connect_realize(|window| {
            resize_window_for_monitor(window);
        });

        glib::idle_add_local_once({
            let search_entry = widgets.search_entry.clone();
            move || {
                search_entry.grab_focus();
            }
        });

        relm4::ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: Self::Input,
        _sender: relm4::ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AppMsg::QueryChanged(query) => {
                self.query = query;
                self.focus_target = FocusTarget::Search;
                self.sync_row_visibility();
                self.request_search();
            }
            AppMsg::SearchEntryKeyPressed(key) => {
                if key == gdk::Key::Down && !self.results.is_empty() {
                    self.focus_target = FocusTarget::Results;
                    if self.selected_index.is_none() {
                        self.selected_index = Some(0);
                    }
                    self.sync_row_visibility();
                }
            }
            AppMsg::ResultsKeyPressed(key) => match key {
                gdk::Key::Down => {
                    self.focus_target = FocusTarget::Results;
                    self.move_selection(1);
                }
                gdk::Key::Up => {
                    if self.selected_index.unwrap_or(0) == 0 {
                        self.focus_target = FocusTarget::Search;
                        self.sync_row_visibility();
                    } else {
                        self.focus_target = FocusTarget::Results;
                        self.move_selection(-1);
                    }
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    self.activate_selected(DEFAULT_ACTION_ID);
                }
                _ => {
                    if let Some(action_id) = self.selected_action_id_for_shortcut(key) {
                        self.activate_selected(action_id);
                    }
                }
            },
            AppMsg::ResultsRowActivated(row_index) => {
                if row_index >= 0 {
                    self.selected_index = Some(row_index as usize);
                    self.focus_target = FocusTarget::Results;
                    self.sync_row_visibility();
                    self.activate_selected(DEFAULT_ACTION_ID);
                }
            }
            AppMsg::RowSelected(row_index) => {
                self.selected_index = row_index.map(|row_index| row_index as usize);
                self.sync_row_visibility();
            }
            AppMsg::ActivateSelected => {
                self.activate_selected(DEFAULT_ACTION_ID);
            }
            AppMsg::SearchFinished { query, result } => {
                if query == self.query {
                    self.apply_search_results(result);
                }
            }
            AppMsg::ActivationFinished(result) => match result {
                Ok(ActivationOutcome::ClosePicker) => root.close(),
                Ok(ActivationOutcome::RefreshResults) => self.request_search(),
                Err(err) => {
                    self.error_message = err;
                }
            },
            AppMsg::Close => root.close(),
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: relm4::ComponentSender<Self>) {
        sync_view(self, widgets);
    }
}

fn sync_view(model: &PickerApp, widgets: &PickerWidgets) {
    if widgets.search_entry.text().as_str() != model.query {
        widgets.search_entry.set_text(&model.query);
    }

    widgets
        .results_label
        .set_text(&format!("{} results", model.results.len()));
    widgets.error_label.set_text(&model.error_message);
    widgets
        .results_stack
        .set_visible_child_name(if model.results.is_empty() {
            RESULTS_STACK_EMPTY
        } else {
            RESULTS_STACK_RESULTS
        });

    if model.results.is_empty() {
        widgets.list_box.unselect_all();
    } else if let Some(selected_index) = model.selected_index {
        if let Some(row) = widgets.list_box.row_at_index(selected_index as i32) {
            widgets.list_box.select_row(Some(&row));
        }
    }

    match model.focus_target {
        FocusTarget::Search => {
            widgets.search_entry.grab_focus_without_selecting();
        }
        FocusTarget::Results if !model.results.is_empty() => {
            if let Some(selected_index) = model.selected_index {
                if let Some(row) = widgets.list_box.row_at_index(selected_index as i32) {
                    widgets.list_box.select_row(Some(&row));
                    row.grab_focus();
                }
            }
        }
        FocusTarget::Results => {
            widgets.search_entry.grab_focus();
        }
    }
}

fn resize_window_for_monitor(window: &ApplicationWindow) {
    let Some(surface) = window.surface() else {
        return;
    };
    let display = surface.display();
    let Some(monitor) = display.monitor_at_surface(&surface) else {
        return;
    };

    let target_height =
        ((monitor.geometry().height() as f64) * WINDOW_HEIGHT_FRACTION).round() as i32;
    window.set_default_size(WINDOW_WIDTH, target_height.max(360));
}

fn row_title(result: &SearchResult) -> String {
    let prefix = match result.kind {
        MatchKind::Application if application_icon_available(result.icon_name.as_deref()) => "",
        MatchKind::Application => "📦 ",
        MatchKind::Notification | MatchKind::Window | MatchKind::Workspace => "",
    };

    format!("{prefix}{}", result.title)
}

fn configure_leading_widgets(result: &SearchResult, icon: &Image, symbol: &Label) {
    icon.set_visible(false);
    symbol.set_visible(false);

    match result.kind {
        MatchKind::Application => {
            if configure_application_image(icon, result.icon_name.as_deref(), RESULT_ICON_SIZE) {
                icon.set_visible(true);
            }
        }
        MatchKind::Notification => {
            symbol.set_label("🔔");
            symbol.set_visible(true);
        }
        MatchKind::Window => {
            symbol.set_label("🗖");
            symbol.set_visible(true);
        }
        MatchKind::Workspace => {
            symbol.set_label("🖥️");
            symbol.set_visible(true);
        }
    }
}

fn configure_subtitle_icon(result: &SearchResult, subtitle_icon: &Image) {
    subtitle_icon.set_visible(false);

    if result.kind == MatchKind::Window
        && configure_application_image(
            subtitle_icon,
            result.icon_name.as_deref(),
            SUBTITLE_ICON_SIZE,
        )
    {
        subtitle_icon.set_visible(true);
    }
}

fn application_icon_available(icon_name: Option<&str>) -> bool {
    let icon_name = match icon_name
        .map(str::trim)
        .filter(|icon_name| !icon_name.is_empty())
    {
        Some(icon_name) => icon_name,
        None => return false,
    };

    if Path::new(icon_name).is_absolute() {
        return Path::new(icon_name).is_file();
    }

    let Some(display) = gdk::Display::default() else {
        return false;
    };
    let icon_theme = gtk::IconTheme::for_display(&display);
    icon_theme.has_icon(icon_name)
}

fn configure_application_image(image: &Image, icon_name: Option<&str>, size: i32) -> bool {
    let icon_name = match icon_name
        .map(str::trim)
        .filter(|icon_name| !icon_name.is_empty())
    {
        Some(icon_name) => icon_name,
        None => return false,
    };

    if Path::new(icon_name).is_absolute() && Path::new(icon_name).is_file() {
        image.set_from_file(Some(icon_name));
    } else {
        let Some(display) = gdk::Display::default() else {
            return false;
        };
        let icon_theme = gtk::IconTheme::for_display(&display);
        if !icon_theme.has_icon(icon_name) {
            return false;
        }
        image.set_icon_name(Some(icon_name));
    }

    image.set_pixel_size(size);
    image.set_valign(gtk::Align::Start);
    true
}

fn format_action_hints(actions: &[ResultAction]) -> String {
    actions
        .iter()
        .map(|action| format!("{} - {}", action.shortcut, action.label))
        .collect::<Vec<_>>()
        .join("   ")
}
