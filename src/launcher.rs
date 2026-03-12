use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, Label, ListBox,
    ListBoxRow, Orientation, PolicyType, ScrolledWindow, SelectionMode,
};
use gtk4 as gtk;

use crate::module::{ActivationOutcome, MatchKind, ModuleRegistry, SearchResult};
use crate::modules;

const WINDOW_WIDTH: i32 = 820;
const WINDOW_HEIGHT_FRACTION: f64 = 0.7;
const WINDOW_CONTENT_WIDTH: i32 = WINDOW_WIDTH - 36;

pub fn run() {
    let app = Application::builder()
        .application_id("com.warren.picky")
        .build();
    app.connect_activate(build_ui);
    app.run();
}

struct UiState {
    registry: ModuleRegistry,
    results: Vec<SearchResult>,
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("picky")
        .default_width(WINDOW_WIDTH)
        .default_height(680)
        .decorated(false)
        .resizable(false)
        .build();

    let container = GtkBox::new(Orientation::Vertical, 10);
    container.set_margin_top(18);
    container.set_margin_bottom(18);
    container.set_margin_start(18);
    container.set_margin_end(18);

    let title = Label::new(Some("picky"));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Center);

    let subtitle = Label::new(Some("Search notifications, applications, and Niri windows"));
    subtitle.add_css_class("dim-label");
    subtitle.set_halign(gtk::Align::Center);

    let search_entry = Entry::new();
    search_entry.set_hexpand(true);
    search_entry.set_placeholder_text(Some("Type to search"));

    let results_label = Label::new(None);
    results_label.set_halign(gtk::Align::Start);

    let list_box = ListBox::new();
    list_box.set_selection_mode(SelectionMode::Single);
    list_box.add_css_class("boxed-list");
    list_box.set_hexpand(true);
    list_box.set_focusable(true);

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&list_box)
        .build();
    scroller.set_min_content_width(WINDOW_CONTENT_WIDTH);
    scroller.set_max_content_width(WINDOW_CONTENT_WIDTH);
    scroller.set_propagate_natural_width(false);

    let error_label = Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.add_css_class("error");

    container.append(&title);
    container.append(&subtitle);
    container.append(&search_entry);
    container.append(&results_label);
    container.append(&scroller);
    container.append(&error_label);
    window.set_child(Some(&container));

    let state = Rc::new(RefCell::new(UiState {
        registry: ModuleRegistry::new(modules::default_modules()),
        results: Vec::new(),
    }));

    refresh_results(&state, "", &list_box, &results_label, &error_label);

    {
        let state = Rc::clone(&state);
        let list_box = list_box.clone();
        let results_label = results_label.clone();
        let error_label = error_label.clone();
        search_entry.connect_changed(move |entry| {
            refresh_results(
                &state,
                entry.text().as_ref(),
                &list_box,
                &results_label,
                &error_label,
            );
        });
    }

    {
        let state = Rc::clone(&state);
        let window = window.clone();
        let list_box = list_box.clone();
        let list_box_for_action = list_box.clone();
        let search_entry = search_entry.clone();
        let results_label = results_label.clone();
        let error_label = error_label.clone();
        list_box.connect_row_activated(move |_, row| {
            activate_row(
                &state,
                row.index(),
                &window,
                &search_entry,
                &list_box_for_action,
                &results_label,
                &error_label,
            );
        });
    }

    {
        let state = Rc::clone(&state);
        let list_box = list_box.clone();
        let window = window.clone();
        let search_entry_for_action = search_entry.clone();
        let list_box_for_action = list_box.clone();
        let results_label = results_label.clone();
        let error_label = error_label.clone();
        search_entry.connect_activate(move |_| {
            if let Some(row) = list_box.selected_row() {
                activate_row(
                    &state,
                    row.index(),
                    &window,
                    &search_entry_for_action,
                    &list_box_for_action,
                    &results_label,
                    &error_label,
                );
            }
        });
    }

    {
        let list_box = list_box.clone();
        let key_controller = EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Down {
                focus_results_list(&list_box);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        search_entry.add_controller(key_controller);
    }

    {
        let state = Rc::clone(&state);
        let list_box = list_box.clone();
        let list_box_for_keys = list_box.clone();
        let search_entry = search_entry.clone();
        let search_entry_for_actions = search_entry.clone();
        let list_box_for_actions = list_box.clone();
        let results_label = results_label.clone();
        let window_for_keys = window.clone();
        let error_label = error_label.clone();
        let key_controller = EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Down => {
                move_selection(&list_box_for_keys, 1);
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                if selected_row_index(&list_box_for_keys).unwrap_or(0) == 0 {
                    search_entry.grab_focus();
                } else {
                    move_selection(&list_box_for_keys, -1);
                }
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                if let Some(row) = list_box_for_keys.selected_row() {
                    activate_row(
                        &state,
                        row.index(),
                        &window_for_keys,
                        &search_entry,
                        &list_box_for_keys,
                        &results_label,
                        &error_label,
                    );
                }
                glib::Propagation::Stop
            }
            _ => {
                if let Some(action_id) =
                    selected_action_id_for_shortcut(&state, &list_box_for_keys, key)
                {
                    if let Some(row) = list_box_for_keys.selected_row() {
                        activate_row_action(
                            &state,
                            row.index(),
                            action_id,
                            &window_for_keys,
                            &search_entry_for_actions,
                            &list_box_for_actions,
                            &results_label,
                            &error_label,
                        );
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        list_box.add_controller(key_controller);
    }

    {
        let window_for_keys = window.clone();
        let key_controller = EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window_for_keys.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key_controller);
    }

    window.present();
    glib::idle_add_local_once({
        let window = window.clone();
        move || resize_window_for_monitor(&window)
    });
    search_entry.grab_focus();
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

fn refresh_results(
    state: &Rc<RefCell<UiState>>,
    query: &str,
    list_box: &ListBox,
    results_label: &Label,
    error_label: &Label,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let mut state = state.borrow_mut();
    match state.registry.search(query) {
        Ok(results) => {
            state.results = results;
            error_label.set_text("");
        }
        Err(err) => {
            state.results.clear();
            error_label.set_text(&err.to_string());
        }
    }

    results_label.set_text(&format!("{} results", state.results.len()));

    if state.results.is_empty() {
        let row = build_row(None, "No matches.", "", None);
        list_box.append(&row);
        return;
    }

    for result in &state.results {
        let row = build_row(
            Some(result.kind),
            &result.title,
            &result.subtitle,
            result.icon_name.as_deref(),
        );
        list_box.append(&row);
    }

    if let Some(row) = list_box.row_at_index(0) {
        list_box.select_row(Some(&row));
    }
}

fn build_row(
    kind: Option<MatchKind>,
    title: &str,
    subtitle: &str,
    icon_name: Option<&str>,
) -> ListBoxRow {
    let row = ListBoxRow::new();
    let content = GtkBox::new(Orientation::Vertical, 4);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);
    content.set_hexpand(true);

    let prefix = match kind {
        Some(MatchKind::Application) => "📦 ",
        Some(MatchKind::Notification) => "🔔 ",
        Some(MatchKind::Window) => "🪟 ",
        None => "",
    };

    let title_text = match icon_name {
        Some(icon_name) if !icon_name.is_empty() => format!("{prefix}{title}  ({icon_name})"),
        _ => format!("{prefix}{title}"),
    };

    let title_label = Label::new(Some(&title_text));
    title_label.set_halign(gtk::Align::Start);
    title_label.set_xalign(0.0);
    title_label.set_hexpand(true);
    title_label.set_ellipsize(EllipsizeMode::End);
    title_label.set_single_line_mode(true);

    let subtitle_label = Label::new(Some(subtitle));
    subtitle_label.set_halign(gtk::Align::Start);
    subtitle_label.set_xalign(0.0);
    subtitle_label.set_hexpand(true);
    subtitle_label.set_ellipsize(EllipsizeMode::End);
    subtitle_label.set_single_line_mode(true);
    subtitle_label.add_css_class("dim-label");

    content.append(&title_label);
    if !subtitle.is_empty() {
        content.append(&subtitle_label);
    }
    row.set_child(Some(&content));
    row
}

fn move_selection(list_box: &ListBox, offset: i32) {
    let current_index = list_box.selected_row().map(|row| row.index()).unwrap_or(0);
    let next_index = (current_index + offset).max(0);

    if let Some(row) = list_box.row_at_index(next_index) {
        list_box.select_row(Some(&row));
    }
}

fn focus_results_list(list_box: &ListBox) {
    if list_box.selected_row().is_none() {
        if let Some(row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&row));
        }
    }

    if let Some(row) = list_box.selected_row() {
        row.grab_focus();
    } else {
        list_box.grab_focus();
    }
}

fn selected_row_index(list_box: &ListBox) -> Option<i32> {
    list_box.selected_row().map(|row| row.index())
}

fn selected_action_id_for_shortcut(
    state: &Rc<RefCell<UiState>>,
    list_box: &ListBox,
    key: gdk::Key,
) -> Option<&'static str> {
    let shortcut = key.to_unicode()?.to_ascii_lowercase();
    let row_index = selected_row_index(list_box)? as usize;

    state
        .borrow()
        .results
        .get(row_index)?
        .actions
        .iter()
        .find(|action| action.shortcut.to_ascii_lowercase() == shortcut)
        .map(|action| action.id)
}

fn activate_row(
    state: &Rc<RefCell<UiState>>,
    row_index: i32,
    window: &ApplicationWindow,
    search_entry: &Entry,
    list_box: &ListBox,
    results_label: &Label,
    error_label: &Label,
) {
    let Some(result) = state.borrow().results.get(row_index as usize).cloned() else {
        return;
    };

    let activation = {
        let mut state = state.borrow_mut();
        state.registry.activate(&result)
    };

    match activation {
        Ok(outcome) => handle_activation_outcome(
            outcome,
            state,
            window,
            search_entry,
            list_box,
            results_label,
            error_label,
        ),
        Err(err) => error_label.set_text(&err.to_string()),
    }
}

fn activate_row_action(
    state: &Rc<RefCell<UiState>>,
    row_index: i32,
    action_id: &str,
    window: &ApplicationWindow,
    search_entry: &Entry,
    list_box: &ListBox,
    results_label: &Label,
    error_label: &Label,
) {
    let Some(result) = state.borrow().results.get(row_index as usize).cloned() else {
        return;
    };

    let activation = {
        let mut state = state.borrow_mut();
        state.registry.activate_action(&result, action_id)
    };

    match activation {
        Ok(outcome) => handle_activation_outcome(
            outcome,
            state,
            window,
            search_entry,
            list_box,
            results_label,
            error_label,
        ),
        Err(err) => error_label.set_text(&err.to_string()),
    }
}

fn handle_activation_outcome(
    outcome: ActivationOutcome,
    state: &Rc<RefCell<UiState>>,
    window: &ApplicationWindow,
    search_entry: &Entry,
    list_box: &ListBox,
    results_label: &Label,
    error_label: &Label,
) {
    match outcome {
        ActivationOutcome::ClosePicker => window.close(),
        ActivationOutcome::RefreshResults => {
            let should_focus_results = !search_entry.has_focus();
            refresh_results(
                state,
                search_entry.text().as_ref(),
                list_box,
                results_label,
                error_label,
            );
            error_label.set_text("");

            if should_focus_results {
                focus_results_list(list_box);
            } else {
                search_entry.grab_focus();
            }
        }
    }
}
