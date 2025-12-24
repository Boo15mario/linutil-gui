use crate::cli::Args;
use crate::theme::Theme;
#[cfg(feature = "tips")]
use crate::tips;
use gtk4 as gtk;
use gtk::prelude::*;
use gtk::glib::source::timeout_add_local;
use gtk::glib::{ControlFlow, Propagation};
use linutil_core::{Command, Config, ListNode, TabList};
#[cfg(unix)]
use nix::unistd::Uid;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use time::{macros::format_description, OffsetDateTime};

const APP_ID: &str = "com.christitustech.linutil";
const ROOT_WARNING: &str = "WARNING: You are running this utility as root!\n\
This means you have full system access and commands can potentially damage your system if used incorrectly.\n\
Please proceed with caution and make sure you understand what each script does before executing it.";

struct AppState {
    tabs: TabList,
    theme: Theme,
    current_tab: usize,
    visit_stack: Vec<linutil_core::ego_tree::NodeId>,
    filter: String,
    entries: Vec<ListEntry>,
    multi_select: bool,
    skip_confirmation: bool,
    _size_bypass: bool,
    pending_auto_execute: Vec<Rc<ListNode>>,
}

#[derive(Clone)]
struct ListEntry {
    node_id: Option<linutil_core::ego_tree::NodeId>,
    node: Option<Rc<ListNode>>,
    has_children: bool,
    is_up_dir: bool,
}

struct CommandRunner {
    output: Arc<Mutex<String>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child_killer: Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>,
    finished: Arc<Mutex<Option<bool>>>,
    _pty_master: Box<dyn MasterPty + Send>,
}

pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let args = Rc::new(args);

    app.connect_activate(move |app| {
        build_ui(app, args.clone());
    });

    app.run();
    Ok(())
}

fn build_ui(app: &gtk::Application, args: Rc<Args>) {
    let tabs = linutil_core::get_tabs(!args.override_validation);
    let root_id = tabs[0].tree.root().id();

    let mut skip_confirmation = args.skip_confirmation;
    let mut size_bypass = args.size_bypass;
    let mut pending_auto_execute = Vec::new();

    if let Some(config_path) = &args.config {
        let config = Config::read_config(config_path, &tabs);
        skip_confirmation = skip_confirmation || config.skip_confirmation;
        size_bypass = size_bypass || config.size_bypass;
        pending_auto_execute = config.auto_execute_commands;
    }

    let state = Rc::new(RefCell::new(AppState {
        tabs,
        theme: args.theme,
        current_tab: 0,
        visit_stack: vec![root_id],
        filter: String::new(),
        entries: Vec::new(),
        multi_select: false,
        skip_confirmation,
        _size_bypass: size_bypass,
        pending_auto_execute,
    }));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(&window_title())
        .default_width(1100)
        .default_height(720)
        .build();

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root_box.set_margin_top(12);
    root_box.set_margin_bottom(12);
    root_box.set_margin_start(12);
    root_box.set_margin_end(12);

    let top_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let back_button = gtk::Button::with_label("Back");
    back_button.update_property(&[
        gtk::accessible::Property::Label("Back"),
        gtk::accessible::Property::Description(
            "Go back to the previous view or clear the current search.",
        ),
    ]);
    let multi_select_toggle = gtk::ToggleButton::with_label("Multi-select");
    multi_select_toggle.update_property(&[
        gtk::accessible::Property::Label("Multi-select"),
        gtk::accessible::Property::Description("Toggle selecting multiple commands at once."),
    ]);
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_hexpand(true);
    search_entry.set_placeholder_text(Some("Search commands"));
    search_entry.update_property(&[
        gtk::accessible::Property::Label("Search commands"),
        gtk::accessible::Property::Description("Type to filter commands by name."),
        gtk::accessible::Property::Placeholder("Search commands"),
    ]);
    let run_button = gtk::Button::with_label("Run");
    run_button.set_sensitive(false);
    run_button.update_property(&[
        gtk::accessible::Property::Label("Run"),
        gtk::accessible::Property::Description("Run the selected command(s)."),
    ]);
    top_bar.append(&back_button);
    top_bar.append(&multi_select_toggle);
    top_bar.append(&search_entry);
    top_bar.append(&run_button);

    let content_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);

    let tab_list = gtk::ListBox::new();
    tab_list.set_selection_mode(gtk::SelectionMode::Single);
    tab_list.add_css_class("tab-list");
    tab_list.set_focusable(true);
    tab_list.update_property(&[
        gtk::accessible::Property::Label("Tab list"),
        gtk::accessible::Property::Description("Select a tab to change command categories."),
    ]);
    let state_ref = state.borrow();
    for tab in state_ref.tabs.iter() {
        let label = gtk::Label::new(Some(&format!(
            "{} {}",
            state_ref.theme.tab_icon(),
            tab.name
        )));
        label.set_xalign(0.0);
        let row = gtk::ListBoxRow::new();
        row.update_property(&[gtk::accessible::Property::Label(&format!(
            "Tab: {}",
            tab.name
        ))]);
        row.set_child(Some(&label));
        tab_list.append(&row);
    }
    drop(state_ref);
    tab_list.select_row(tab_list.row_at_index(0).as_ref());

    let tab_scroll = gtk::ScrolledWindow::new();
    tab_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tab_scroll.set_min_content_width(240);
    tab_scroll.set_vexpand(true);
    tab_scroll.set_child(Some(&tab_list));

    let right_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    right_box.set_hexpand(true);
    right_box.set_vexpand(true);
    let path_label = gtk::Label::new(None);
    path_label.set_xalign(0.0);
    path_label.add_css_class("path-label");
    path_label.update_property(&[
        gtk::accessible::Property::Label("Current path"),
        gtk::accessible::Property::Description("Shows the current category path."),
    ]);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.set_focusable(true);
    list_box.update_property(&[
        gtk::accessible::Property::Label("Command list"),
        gtk::accessible::Property::Description("Select a command to view details and run it."),
    ]);
    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    list_scroll.set_hexpand(true);
    list_scroll.set_vexpand(true);
    list_scroll.set_child(Some(&list_box));

    let info_label = gtk::Label::new(Some("Select a command to view its description."));
    info_label.set_xalign(0.0);
    info_label.set_wrap(true);
    info_label.update_property(&[
        gtk::accessible::Property::Label("Command description"),
        gtk::accessible::Property::Description("Displays details about the selected command."),
    ]);

    #[cfg(feature = "tips")]
    let tip_label = {
        let tip = tips::get_random_tip();
        let label = gtk::Label::new(Some(&format!("Tip: {tip}")));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.update_property(&[
            gtk::accessible::Property::Label("Tip"),
            gtk::accessible::Property::Description("Displays a usage tip."),
        ]);
        label
    };

    right_box.append(&path_label);
    right_box.append(&list_scroll);
    right_box.append(&info_label);
    #[cfg(feature = "tips")]
    right_box.append(&tip_label);

    content_box.append(&tab_scroll);
    content_box.append(&right_box);
    root_box.append(&top_bar);
    root_box.append(&content_box);
    window.set_child(Some(&root_box));

    refresh_list(
        state.clone(),
        &list_box,
        &path_label,
        &run_button,
        &back_button,
        &info_label,
    );

    #[cfg(unix)]
    if !args.bypass_root && Uid::effective().is_root() {
        show_info_dialog(window.upcast_ref(), "Root User Warning", ROOT_WARNING);
    }

    let state_clone = state.clone();
    let list_box_clone = list_box.clone();
    let path_label_clone = path_label.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let info_label_clone = info_label.clone();
    tab_list.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let mut state = state_clone.borrow_mut();
        let new_tab = row.index() as usize;
        if new_tab == state.current_tab {
            return;
        }
        state.current_tab = new_tab;
        state.visit_stack.clear();
        let root_id = state.tabs[new_tab].tree.root().id();
        state.visit_stack.push(root_id);
        state.filter.clear();
        drop(state);
        refresh_list(
            state_clone.clone(),
            &list_box_clone,
            &path_label_clone,
            &run_button_clone,
            &back_button_clone,
            &info_label_clone,
        );
    });

    let state_clone = state.clone();
    let list_box_clone = list_box.clone();
    let path_label_clone = path_label.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let info_label_clone = info_label.clone();
    search_entry.connect_changed(move |entry| {
        let mut state = state_clone.borrow_mut();
        state.filter = entry.text().trim().to_string();
        drop(state);
        refresh_list(
            state_clone.clone(),
            &list_box_clone,
            &path_label_clone,
            &run_button_clone,
            &back_button_clone,
            &info_label_clone,
        );
    });

    let state_clone = state.clone();
    let list_box_clone = list_box.clone();
    let path_label_clone = path_label.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let info_label_clone = info_label.clone();
    let search_entry_clone = search_entry.clone();
    back_button.connect_clicked(move |_| {
        let mut state = state_clone.borrow_mut();
        if !state.filter.is_empty() {
            state.filter.clear();
            search_entry_clone.set_text("");
        } else if state.visit_stack.len() > 1 {
            state.visit_stack.pop();
        }
        drop(state);
        refresh_list(
            state_clone.clone(),
            &list_box_clone,
            &path_label_clone,
            &run_button_clone,
            &back_button_clone,
            &info_label_clone,
        );
    });

    let state_clone = state.clone();
    let list_box_clone = list_box.clone();
    let path_label_clone = path_label.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let info_label_clone = info_label.clone();
    multi_select_toggle.connect_toggled(move |toggle| {
        let mut state = state_clone.borrow_mut();
        state.multi_select = toggle.is_active();
        drop(state);
        refresh_list(
            state_clone.clone(),
            &list_box_clone,
            &path_label_clone,
            &run_button_clone,
            &back_button_clone,
            &info_label_clone,
        );
    });

    let state_clone = state.clone();
    let info_label_clone = info_label.clone();
    let run_button_clone = run_button.clone();
    list_box.connect_selected_rows_changed(move |list| {
        let state = state_clone.borrow();
        let (desc, has_command) = describe_selection(&state, &list.selected_rows());
        run_button_clone.set_sensitive(has_command);
        info_label_clone.set_text(desc.as_deref().unwrap_or("Select a command to view its description."));
    });

    let search_entry_clone = search_entry.clone();
    let list_box_clone = list_box.clone();
    let tab_list_clone = tab_list.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        let alt = modifiers.contains(gtk::gdk::ModifierType::ALT_MASK);
        let key_char = key.to_unicode().map(|c| c.to_ascii_lowercase());

        if ctrl && key_char == Some('f') {
            search_entry_clone.grab_focus();
            search_entry_clone.select_region(0, -1);
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('l') {
            list_box_clone.grab_focus();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('t') {
            tab_list_clone.grab_focus();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('r') {
            run_button_clone.emit_clicked();
            return Propagation::Stop;
        }
        if alt && key.name().as_deref() == Some("Left") {
            back_button_clone.emit_clicked();
            return Propagation::Stop;
        }
        if key.name().as_deref() == Some("Escape") {
            if !search_entry_clone.text().is_empty() {
                search_entry_clone.set_text("");
                search_entry_clone.grab_focus();
                return Propagation::Stop;
            }
        }
        Propagation::Proceed
    });
    window.add_controller(key_controller);

    let state_clone = state.clone();
    let window_clone = window.clone();
    let list_box_clone = list_box.clone();
    run_button.connect_clicked(move |_| {
        let state = state_clone.borrow();
        let selection = list_box_clone.selected_rows();
        let (commands, rejected) = collect_selected_commands(&state, &selection);
        drop(state);
        if !rejected.is_empty() {
            show_info_dialog(
                window_clone.upcast_ref(),
                "Some commands were skipped",
                &format!(
                    "These commands do not support multi-select and were skipped:\n{}",
                    rejected.join(", ")
                ),
            );
        }
        if commands.is_empty() {
            show_info_dialog(
                window_clone.upcast_ref(),
                "No command selected",
                "Select a command to run.",
            );
            return;
        }
        let skip_confirmation = state_clone.borrow().skip_confirmation;
        confirm_and_run(window_clone.upcast_ref(), commands, skip_confirmation);
    });

    let state_clone = state.clone();
    let window_clone = window.clone();
    let list_box_clone = list_box.clone();
    let path_label_clone = path_label.clone();
    let run_button_clone = run_button.clone();
    let back_button_clone = back_button.clone();
    let info_label_clone = info_label.clone();
    list_box.connect_row_activated(move |_, row| {
        let mut state = state_clone.borrow_mut();
        let idx = row.index() as usize;
        let Some(entry) = state.entries.get(idx).cloned() else { return };
        if entry.is_up_dir {
            if state.visit_stack.len() > 1 {
                state.visit_stack.pop();
            }
            drop(state);
            refresh_list(
                state_clone.clone(),
                &list_box_clone,
                &path_label_clone,
                &run_button_clone,
                &back_button_clone,
                &info_label_clone,
            );
            return;
        }
        if entry.has_children && state.filter.is_empty() {
            if let Some(node_id) = entry.node_id {
                state.visit_stack.push(node_id);
            }
            drop(state);
            refresh_list(
                state_clone.clone(),
                &list_box_clone,
                &path_label_clone,
                &run_button_clone,
                &back_button_clone,
                &info_label_clone,
            );
            return;
        }
        let Some(node) = entry.node else { return };
        drop(state);
        let skip_confirmation = state_clone.borrow().skip_confirmation;
        confirm_and_run(window_clone.upcast_ref(), vec![node], skip_confirmation);
    });

    let state_clone = state.clone();
    let window_clone = window.clone();
    gtk::glib::idle_add_local_once(move || {
        let mut state = state_clone.borrow_mut();
        if !state.pending_auto_execute.is_empty() {
            let commands = std::mem::take(&mut state.pending_auto_execute);
            let skip_confirmation = state.skip_confirmation;
            drop(state);
            confirm_and_run(window_clone.upcast_ref(), commands, skip_confirmation);
        }
    });

    window.show();
}

fn window_title() -> String {
    format!(
        "Linux Toolbox - {}",
        env!("CARGO_PKG_VERSION")
    )
}

fn refresh_list(
    state: Rc<RefCell<AppState>>,
    list_box: &gtk::ListBox,
    path_label: &gtk::Label,
    run_button: &gtk::Button,
    back_button: &gtk::Button,
    info_label: &gtk::Label,
) {
    let (entries, theme, multi_select, path_text, back_enabled) = {
        let mut state = state.borrow_mut();
        build_entries(&mut state);
        let entries = state.entries.clone();
        let theme = state.theme;
        let multi_select = state.multi_select;
        let path_text = path_label_text(&state);
        let back_enabled = !state.filter.is_empty() || state.visit_stack.len() > 1;
        (entries, theme, multi_select, path_text, back_enabled)
    };

    clear_list_box(list_box);
    for entry in &entries {
        let label = gtk::Label::new(Some(&format_entry(theme, multi_select, entry)));
        label.set_xalign(0.0);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&label));
        list_box.append(&row);
    }

    list_box.set_selection_mode(if multi_select {
        gtk::SelectionMode::Multiple
    } else {
        gtk::SelectionMode::Single
    });

    path_label.set_text(&path_text);
    back_button.set_sensitive(back_enabled);
    run_button.set_sensitive(false);
    info_label.set_text("Select a command to view its description.");
}

fn build_entries(state: &mut AppState) {
    state.entries.clear();
    if state.filter.is_empty() {
        if state.visit_stack.len() > 1 {
            state.entries.push(ListEntry {
                node_id: None,
                node: None,
                has_children: false,
                is_up_dir: true,
            });
        }
        let node_id = *state.visit_stack.last().unwrap();
        let tree = &state.tabs[state.current_tab].tree;
        let node = tree.get(node_id).unwrap();
        for child in node.children() {
            state.entries.push(ListEntry {
                node_id: Some(child.id()),
                node: Some(child.value().clone()),
                has_children: child.has_children(),
                is_up_dir: false,
            });
        }
    } else {
        let query = state.filter.to_lowercase();
        for tab in state.tabs.iter() {
            let mut stack = vec![tab.tree.root().id()];
            while let Some(node_id) = stack.pop() {
                let node = tab.tree.get(node_id).unwrap();
                if node.value().name.to_lowercase().contains(&query) && !node.has_children() {
                    state.entries.push(ListEntry {
                        node_id: Some(node.id()),
                        node: Some(node.value().clone()),
                        has_children: false,
                        is_up_dir: false,
                    });
                }
                stack.extend(node.children().map(|child| child.id()));
            }
        }
        state
            .entries
            .sort_by(|a, b| a.node.as_ref().unwrap().name.cmp(&b.node.as_ref().unwrap().name));
    }
}

fn format_entry(theme: Theme, multi_select: bool, entry: &ListEntry) -> String {
    if entry.is_up_dir {
        return ".. (Up)".to_string();
    }
    let Some(node) = &entry.node else { return String::new() };
    if entry.has_children {
        format!("{} {}", theme.dir_icon(), node.name)
    } else if multi_select && !node.multi_select {
        format!("{} {} (single only)", theme.cmd_icon(), node.name)
    } else {
        format!("{} {}", theme.cmd_icon(), node.name)
    }
}

fn path_label_text(state: &AppState) -> String {
    if !state.filter.is_empty() {
        return "Search results".to_string();
    }
    let tab_name = &state.tabs[state.current_tab].name;
    let tree = &state.tabs[state.current_tab].tree;
    let mut parts = vec![tab_name.clone()];
    for node_id in state.visit_stack.iter().skip(1) {
        if let Some(node) = tree.get(*node_id) {
            parts.push(node.value().name.clone());
        }
    }
    parts.join(" / ")
}

fn describe_selection(
    state: &AppState,
    rows: &[gtk::ListBoxRow],
) -> (Option<String>, bool) {
    if rows.is_empty() {
        return (None, false);
    }
    let mut has_command = false;
    for row in rows {
        let idx = row.index() as usize;
        let Some(entry) = state.entries.get(idx) else { continue };
        if entry.is_up_dir || entry.has_children {
            continue;
        }
        if let Some(node) = &entry.node {
            has_command = true;
            let desc = if node.description.is_empty() {
                format!("Command: {}", node.name)
            } else {
                format!("{}: {}", node.name, node.description)
            };
            return (Some(desc), has_command);
        }
    }
    (None, has_command)
}

fn collect_selected_commands(
    state: &AppState,
    rows: &[gtk::ListBoxRow],
) -> (Vec<Rc<ListNode>>, Vec<String>) {
    let mut commands = Vec::new();
    let mut rejected = Vec::new();
    let multiple = rows.len() > 1;

    for row in rows {
        let idx = row.index() as usize;
        let Some(entry) = state.entries.get(idx) else { continue };
        if entry.is_up_dir || entry.has_children {
            continue;
        }
        let Some(node) = &entry.node else { continue };
        if multiple && !node.multi_select {
            rejected.push(node.name.clone());
        } else {
            commands.push(node.clone());
        }
    }
    (commands, rejected)
}

fn confirm_and_run(parent: &gtk::Window, commands: Vec<Rc<ListNode>>, skip: bool) {
    if skip {
        if let Some(app) = parent.application() {
            open_command_window(&app, commands);
        }
        return;
    }

    let names = commands
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!("Run the following command(s)?\n{names}");
    let parent = parent.clone();
    let parent_clone = parent.clone();
    let (dialog, run_button, cancel_button) =
        build_confirmation_dialog(&parent_clone, "Confirm Commands", &message);
    let dialog_clone = dialog.clone();
    let commands_clone = commands.clone();
    run_button.connect_clicked(move |_| {
        dialog_clone.close();
        if let Some(app) = parent_clone.application() {
            open_command_window(&app, commands_clone.clone());
        }
    });
    let dialog_clone = dialog.clone();
    cancel_button.connect_clicked(move |_| {
        dialog_clone.close();
    });
}

fn build_confirmation_dialog(
    parent: &gtk::Window,
    title: &str,
    message: &str,
) -> (gtk::Window, gtk::Button, gtk::Button) {
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(420)
        .default_height(180)
        .build();
    dialog.set_accessible_role(gtk::AccessibleRole::AlertDialog);
    dialog.update_property(&[
        gtk::accessible::Property::Label(title),
        gtk::accessible::Property::Description(message),
    ]);

    let box_root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_root.set_margin_top(12);
    box_root.set_margin_bottom(12);
    box_root.set_margin_start(12);
    box_root.set_margin_end(12);
    let label = gtk::TextView::new();
    label.set_editable(false);
    label.set_cursor_visible(false);
    label.set_wrap_mode(gtk::WrapMode::WordChar);
    label.set_focusable(true);
    label.set_accessible_role(gtk::AccessibleRole::TextBox);
    label.buffer().set_text(message);
    label.update_property(&[
        gtk::accessible::Property::Label("Commands to run"),
        gtk::accessible::Property::Description(message),
        gtk::accessible::Property::ReadOnly(true),
    ]);

    let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_box.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::with_label("Run");
    cancel.update_property(&[
        gtk::accessible::Property::Label("Cancel"),
        gtk::accessible::Property::Description("Cancel running the selected commands."),
    ]);
    run.update_property(&[
        gtk::accessible::Property::Label("Run"),
        gtk::accessible::Property::Description("Run the selected commands."),
    ]);
    button_box.append(&cancel);
    button_box.append(&run);

    box_root.append(&label);
    box_root.append(&button_box);
    dialog.set_child(Some(&box_root));
    dialog.update_relation(&[
        gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()]),
        gtk::accessible::Relation::DescribedBy(&[label.upcast_ref()]),
    ]);
    dialog.set_default_widget(Some(&run));
    gtk::prelude::GtkWindowExt::set_focus(&dialog, Some(&label));
    dialog.show();
    (dialog, run, cancel)
}

fn show_info_dialog(parent: &gtk::Window, title: &str, message: &str) {
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(420)
        .default_height(180)
        .build();
    dialog.set_accessible_role(gtk::AccessibleRole::AlertDialog);
    dialog.update_property(&[
        gtk::accessible::Property::Label(title),
        gtk::accessible::Property::Description(message),
    ]);

    let box_root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_root.set_margin_top(12);
    box_root.set_margin_bottom(12);
    box_root.set_margin_start(12);
    box_root.set_margin_end(12);
    let label = gtk::TextView::new();
    label.set_editable(false);
    label.set_cursor_visible(false);
    label.set_wrap_mode(gtk::WrapMode::WordChar);
    label.set_focusable(true);
    label.set_accessible_role(gtk::AccessibleRole::TextBox);
    label.buffer().set_text(message);
    label.update_property(&[
        gtk::accessible::Property::Label(title),
        gtk::accessible::Property::Description(message),
        gtk::accessible::Property::ReadOnly(true),
    ]);
    dialog.update_relation(&[
        gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()]),
        gtk::accessible::Relation::DescribedBy(&[label.upcast_ref()]),
    ]);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    box_root.append(&label);
    box_root.append(&close);
    dialog.set_child(Some(&box_root));
    let dialog_clone = dialog.clone();
    close.connect_clicked(move |_| dialog_clone.close());
    dialog.show();
}

fn open_command_window(app: &gtk::Application, commands: Vec<Rc<ListNode>>) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Command Output")
        .default_width(900)
        .default_height(600)
        .build();

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root_box.set_hexpand(true);
    root_box.set_vexpand(true);
    root_box.set_margin_top(12);
    root_box.set_margin_bottom(12);
    root_box.set_margin_start(12);
    root_box.set_margin_end(12);

    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let status_label = gtk::Label::new(Some("Running..."));
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);
    status_label.update_property(&[gtk::accessible::Property::Label("Command status")]);
    let stop_button = gtk::Button::with_label("Stop");
    let save_button = gtk::Button::with_label("Save Log");
    let close_button = gtk::Button::with_label("Close");
    stop_button.update_property(&[
        gtk::accessible::Property::Label("Stop"),
        gtk::accessible::Property::Description("Stop the running command."),
    ]);
    save_button.update_property(&[
        gtk::accessible::Property::Label("Save log"),
        gtk::accessible::Property::Description("Save the command output to a file."),
    ]);
    close_button.update_property(&[gtk::accessible::Property::Label("Close")]);
    status_box.append(&status_label);
    status_box.append(&stop_button);
    status_box.append(&save_button);
    status_box.append(&close_button);

    let output_view = gtk::TextView::new();
    output_view.set_monospace(true);
    output_view.set_editable(false);
    output_view.update_property(&[
        gtk::accessible::Property::Label("Command output"),
        gtk::accessible::Property::Description("Live output from the command."),
    ]);
    let output_scroll = gtk::ScrolledWindow::new();
    output_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    output_scroll.set_hexpand(true);
    output_scroll.set_vexpand(true);
    output_scroll.set_child(Some(&output_view));

    let input_entry = gtk::Entry::new();
    input_entry.set_placeholder_text(Some("Type input for the command and press Enter"));
    input_entry.update_property(&[
        gtk::accessible::Property::Label("Command input"),
        gtk::accessible::Property::Description(
            "Type input and press Enter to send it to the command.",
        ),
        gtk::accessible::Property::Placeholder("Type input for the command and press Enter"),
    ]);

    root_box.append(&status_box);
    root_box.append(&output_scroll);
    root_box.append(&input_entry);
    window.set_child(Some(&root_box));

    let output_buffer = output_view.buffer();
    let runner = Rc::new(RefCell::new(CommandRunner::spawn(&commands)));
    let last_len = Rc::new(RefCell::new(0usize));
    let output_buffer_clone = output_buffer.clone();
    let output_view_clone = output_view.clone();
    let status_label_clone = status_label.clone();
    let stop_button_clone = stop_button.clone();
    let input_entry_clone = input_entry.clone();
    let runner_clone = runner.clone();
    let last_len_clone = last_len.clone();
    timeout_add_local(Duration::from_millis(50), move || {
        let mut offset = last_len_clone.borrow_mut();
        let chunk = runner_clone.borrow().read_output_since(&mut offset);
        if !chunk.is_empty() {
            let mut end = output_buffer_clone.end_iter();
            output_buffer_clone.insert(&mut end, &chunk);
            let mut end = output_buffer_clone.end_iter();
            output_view_clone.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
        }

        if let Some(success) = runner_clone.borrow().finished() {
            if success {
                status_label_clone.set_text("Finished successfully.");
            } else {
                status_label_clone.set_text("Finished with errors.");
            }
            stop_button_clone.set_sensitive(false);
            input_entry_clone.set_sensitive(false);
            return ControlFlow::Break;
        }

        ControlFlow::Continue
    });

    let runner_clone = runner.clone();
    stop_button.connect_clicked(move |_| {
        runner_clone.borrow_mut().kill();
    });

    let runner_clone = runner.clone();
    input_entry.connect_activate(move |entry| {
        let text = entry.text().to_string();
        entry.set_text("");
        if !text.is_empty() {
            runner_clone.borrow().send_input(&format!("{text}\n"));
        }
    });

    let runner_clone = runner.clone();
    let status_label_clone = status_label.clone();
    save_button.connect_clicked(move |_| match runner_clone.borrow().save_log() {
        Ok(path) => status_label_clone.set_text(&format!("Saved log to {path}")),
        Err(err) => status_label_clone.set_text(&format!("Failed to save log: {err}")),
    });

    let window_clone = window.clone();
    close_button.connect_clicked(move |_| window_clone.close());

    let input_entry_clone = input_entry.clone();
    let output_view_clone = output_view.clone();
    let stop_button_clone = stop_button.clone();
    let save_button_clone = save_button.clone();
    let close_button_clone = close_button.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        let key_char = key.to_unicode().map(|c| c.to_ascii_lowercase());

        if ctrl && key_char == Some('s') {
            save_button_clone.emit_clicked();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('w') {
            close_button_clone.emit_clicked();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('k') {
            stop_button_clone.emit_clicked();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('i') {
            input_entry_clone.grab_focus();
            return Propagation::Stop;
        }
        if ctrl && key_char == Some('o') {
            output_view_clone.grab_focus();
            return Propagation::Stop;
        }
        Propagation::Proceed
    });
    window.add_controller(key_controller);

    window.show();
}

impl CommandRunner {
    fn spawn(commands: &[Rc<ListNode>]) -> Self {
        let pty_system = NativePtySystem::default();
        let mut cmd: CommandBuilder = CommandBuilder::new("sh");
        cmd.arg("-c");

        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("FORCE_COLOR", "1");
        cmd.env("NO_COLOR", "");

        let mut script = String::new();
        for node in commands {
            match &node.command {
                Command::Raw(prompt) => {
                    script.push_str(prompt);
                    script.push('\n');
                }
                Command::LocalFile { executable, args, file } => {
                    if let Some(parent) = file.parent() {
                        script.push_str(&format!("cd {}\n", parent.display()));
                    }
                    script.push_str(executable);
                    for arg in args {
                        script.push(' ');
                        script.push_str(arg);
                    }
                    script.push('\n');
                }
                Command::None => {}
            }
        }

        cmd.arg(script);

        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let child_killer = child.clone_killer();
        let output = Arc::new(Mutex::new(String::new()));
        let output_clone = output.clone();
        let finished = Arc::new(Mutex::new(None));
        let finished_clone = finished.clone();

        let mut reader = pair.master.try_clone_reader().unwrap();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(size) if size == 0 => break,
                    Ok(size) => {
                        let chunk = String::from_utf8_lossy(&buf[..size]).to_string();
                        let chunk = strip_ansi(&chunk);
                        if !chunk.is_empty() {
                            if let Ok(mut output) = output_clone.lock() {
                                output.push_str(&chunk);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        thread::spawn(move || {
            let status = child.wait().unwrap();
            if let Ok(mut finished) = finished_clone.lock() {
                *finished = Some(status.success());
            }
        });

        let writer = pair.master.take_writer().unwrap();

        Self {
            output,
            writer: Arc::new(Mutex::new(writer)),
            child_killer: Arc::new(Mutex::new(Some(child_killer))),
            finished,
            _pty_master: pair.master,
        }
    }

    fn send_input(&self, input: &str) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(input.as_bytes());
            let _ = writer.flush();
        }
    }

    fn kill(&mut self) {
        if let Ok(mut killer) = self.child_killer.lock() {
            if let Some(mut killer) = killer.take() {
                let _ = killer.kill();
            }
        }
    }

    fn save_log(&self) -> Result<String, std::io::Error> {
        let mut log_path = std::env::temp_dir();
        let date_format = format_description!("[year]-[month]-[day]-[hour]-[minute]-[second]");
        log_path.push(format!(
            "linutil_log_{}.log",
            OffsetDateTime::now_local()
                .unwrap_or(OffsetDateTime::now_utc())
                .format(&date_format)
                .unwrap()
        ));

        let output = self.output.lock().unwrap();
        std::fs::write(&log_path, output.as_str())?;
        Ok(log_path.to_string_lossy().into_owned())
    }

    fn read_output_since(&self, offset: &mut usize) -> String {
        let output = self.output.lock().unwrap();
        if *offset >= output.len() {
            return String::new();
        }
        let chunk = output[*offset..].to_string();
        *offset = output.len();
        chunk
    }

    fn finished(&self) -> Option<bool> {
        let finished = self.finished.lock().unwrap();
        *finished
    }
}

fn strip_ansi(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(next) = chars.next() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        result.push(ch);
    }
    result
}

fn clear_list_box(list_box: &gtk::ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
}
