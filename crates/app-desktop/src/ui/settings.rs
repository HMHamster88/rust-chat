//! The settings screen: sidebar navigation, section cards and the danger bar.

use super::*;

pub(super) fn build_settings_screen(settings: &ClientSettings) -> SettingsScreen {
    let devices = rust_chat_media_desktop::audio::audio_devices();
    let inputs = device_choices(devices.inputs);
    let outputs = device_choices(devices.outputs);
    let input_index = device_index(&inputs, settings.mic_device.as_deref());
    let output_index = device_index(&outputs, settings.speaker_device.as_deref());

    let open_room = icons::icon_button(icons::Icon::Settings, "Settings");
    let open_connect = icons::icon_button(icons::Icon::Settings, "Settings");
    let close = icons::icon_button(icons::Icon::Close, "Close settings");
    let status = ui_kit::muted("", 12.0);

    // --- controls ---
    let auto_connect = CheckBox::new();
    auto_connect.set_is_checked(Some(settings.auto_connect));
    let forget_pin = icons::icon_button(icons::Icon::Shield, "Forget pinned key");

    let timestamps = CheckBox::new();
    timestamps.set_is_checked(Some(settings.show_timestamps));
    let clock24 = CheckBox::new();
    clock24.set_is_checked(Some(settings.clock_24h));
    let keep_history = CheckBox::new();
    keep_history.set_is_checked(Some(settings.keep_history));
    let clear_history = icons::icon_button(icons::Icon::Delete, "Clear local history");

    let mic = small_button("Microphone: system default");
    let speaker = small_button("Speakers: system default");
    let quality = small_button("Default quality");

    let downloads = ui_kit::muted("", 12.0);
    let choose_downloads = icons::icon_button(icons::Icon::Folder, "Choose the downloads folder");
    let reset_downloads =
        icons::icon_button(icons::Icon::Restart, "Use the default Downloads folder");
    let downloads_buttons = ui_kit::row(6.0);
    downloads_buttons.children().add(&choose_downloads);
    downloads_buttons.children().add(&reset_downloads);
    let open_when_finished = CheckBox::new();
    open_when_finished.set_is_checked(Some(settings.open_when_finished));

    let about_version = ui_kit::muted("", 12.0);
    let about_data = ui_kit::muted("", 12.0);

    // --- sections ---
    let mut sections: Vec<Ref<StackPanel>> = Vec::new();
    let mut nav: Vec<Ref<Button>> = Vec::new();

    let connection = settings_section(
        icons::Icon::Server,
        "Connection",
        "chat.example.org:8444 · TLS · your key is approved once",
        vec![
            settings_row(
                "Connect automatically on start",
                Some("Uses the password from Credential Manager"),
                &auto_connect,
            ),
            settings_row(
                "Pinned server key",
                Some("Clients approve the certificate on first connect"),
                &forget_pin,
            ),
        ],
    );
    let chat = settings_section(
        icons::Icon::Chat,
        "Chat",
        "Applies immediately, mid-conversation.",
        vec![
            settings_row(
                "Show timestamps",
                Some("Local time next to each message"),
                &timestamps,
            ),
            settings_row(
                "24-hour clock",
                Some("Otherwise 12-hour with AM/PM"),
                &clock24,
            ),
            settings_row(
                "Keep history on disk",
                Some("Plain JSONL, one file per server and room"),
                &keep_history,
            ),
            settings_row(
                "Local history",
                Some("Deletes the saved transcript of the current room"),
                &clear_history,
            ),
        ],
    );
    let audio = settings_section(
        icons::Icon::Microphone,
        "Audio & video",
        "Devices and the default share quality.",
        vec![
            settings_row(
                "Microphone",
                Some("System default or a specific input device"),
                &mic,
            ),
            settings_row("Speakers", Some("Where incoming voice is played"), &speaker),
            settings_row(
                "Screen share quality",
                Some("Resolution, frame rate and target bitrate"),
                &quality,
            ),
        ],
    );
    let files = settings_section(
        icons::Icon::Folder,
        "Files",
        "Received files and what happens when a transfer completes.",
        vec![
            settings_row("Download folder", None, &downloads),
            settings_row(
                "Change folder",
                Some("Pick another folder or return to the OS default"),
                &downloads_buttons,
            ),
            settings_row(
                "Open received files when finished",
                Some("Only after the SHA-256 matches"),
                &open_when_finished,
            ),
        ],
    );
    let about = settings_section(
        icons::Icon::Info,
        "About",
        "Version, update checks and where the app keeps its data.",
        vec![
            settings_row("Version", None, &about_version),
            settings_row("Settings and history", None, &about_data),
        ],
    );
    for (glyph, title, panel) in [
        (icons::Icon::Server, "Connection", &connection),
        (icons::Icon::Chat, "Chat", &chat),
        (icons::Icon::Microphone, "Audio & video", &audio),
        (icons::Icon::Folder, "Files", &files),
        (icons::Icon::Info, "About", &about),
    ] {
        let content = icons::icon_label(glyph, title);
        let button = Button::new();
        button.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
        button.set_content(Some(Rc::new(content) as ferroui_base::BoxedValue));
        button.set_horizontal_content_alignment(HorizontalAlignment::Left);
        button.set_horizontal_alignment(HorizontalAlignment::Stretch);
        nav.push(button);
        sections.push(panel.clone());
    }

    // --- danger bar + footer ---
    let confirm_text = ui_kit::body("", 12.5);
    let confirm = icons::icon_button(icons::Icon::Check, "Confirm");
    let cancel = icons::icon_button(icons::Icon::Close, "Cancel");
    confirm.set_is_visible(false);
    cancel.set_is_visible(false);
    let danger_row = ui_kit::row(8.0);
    danger_row.children().add(&confirm_text);
    danger_row.children().add(&confirm);
    danger_row.children().add(&cancel);
    let danger_bar = Border::new();
    danger_bar.set_background(Some(ui_kit::brush(0xFFFD_F3F2)));
    danger_bar.set_border_brush(Some(ui_kit::brush(0xFFF0_C9C4)));
    danger_bar.set_border_thickness(Thickness::uniform(1.0));
    danger_bar.set_corner_radius(CornerRadius::uniform(8.0));
    danger_bar.set_padding(Thickness {
        left: 12.0,
        top: 8.0,
        right: 12.0,
        bottom: 8.0,
    });
    danger_bar.set_child(&danger_row);
    danger_bar.set_horizontal_alignment(HorizontalAlignment::Stretch);
    danger_bar.set_is_visible(false);
    danger_bar.set_margin(Thickness {
        left: 0.0,
        top: 4.0,
        right: 0.0,
        bottom: 0.0,
    });

    // --- body: one section visible at a time, inside a scroller ---
    let body = StackPanel::new();
    body.set_spacing(10.0);
    for section in &sections {
        section.set_is_visible(false);
        body.children().add(section);
    }
    body.children().add(&danger_bar);
    let scroller = ScrollViewer::new();
    scroller.set_vertical_scroll_bar_visibility(ScrollBarVisibility::Auto);
    scroller.set_horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled);
    let body_frame = Border::new();
    body_frame.set_padding(Thickness {
        left: 20.0,
        top: 16.0,
        right: 20.0,
        bottom: 16.0,
    });
    body_frame.set_child(&body);
    scroller.set_content(Some(Control::boxed(&body_frame)));

    // --- header, nav, footer ---
    let heading = ui_kit::headline("Settings");
    let header = ui_kit::row(8.0);
    header.children().add(&heading);
    let header_frame = Border::new();
    header_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    header_frame.set_padding(Thickness {
        left: 20.0,
        top: 10.0,
        right: 14.0,
        bottom: 10.0,
    });
    header_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    header_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 1.0,
    });
    header_frame.set_child(&header);

    let nav_panel = StackPanel::new();
    nav_panel.set_spacing(2.0);
    nav_panel.children().add(ui_kit::section_label("SETTINGS"));
    for button in &nav {
        nav_panel.children().add(button);
    }
    nav_panel.children().add(ui_kit::divider());
    nav_panel.children().add(&close);
    let nav_frame = Border::new();
    nav_frame.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
    nav_frame.set_padding(Thickness {
        left: 10.0,
        top: 14.0,
        right: 10.0,
        bottom: 14.0,
    });
    nav_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    nav_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 0.0,
    });
    nav_frame.set_child(&nav_panel);

    let footer = ui_kit::row(10.0);
    footer.children().add(&status);
    let footer_frame = Border::new();
    footer_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    footer_frame.set_padding(Thickness {
        left: 20.0,
        top: 8.0,
        right: 20.0,
        bottom: 8.0,
    });
    footer_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    footer_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 1.0,
        right: 0.0,
        bottom: 0.0,
    });
    footer_frame.set_child(&footer);

    let content = Grid::new();
    content.set_row_definitions(RowDefinitions::from_items([
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::new(1.0, GridUnitType::Star)),
        RowDefinition::with_height(GridLength::AUTO),
    ]));
    Grid::set_row(&header_frame, 0);
    Grid::set_row(&scroller, 1);
    Grid::set_row(&footer_frame, 2);
    content.children().add(&header_frame);
    content.children().add(&scroller);
    content.children().add(&footer_frame);

    let view = Grid::new();
    view.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(210.0, GridUnitType::Pixel)),
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
    ]));
    Grid::set_column(&nav_frame, 0);
    Grid::set_column(&content, 1);
    view.children().add(&nav_frame);
    view.children().add(&content);
    view.set_is_visible(false);

    SettingsScreen {
        view,
        nav,
        sections,
        section: Cell::new(0),
        danger_bar: danger_bar.clone(),
        about_slot: about.clone(),
        open_room,
        open_connect,
        close,
        status,
        auto_connect,
        timestamps,
        clock24,
        keep_history,
        open_when_finished,
        mic,
        speaker,
        quality,
        downloads,
        choose_downloads,
        reset_downloads,
        about_version,
        about_data,
        forget_pin,
        clear_history,
        pending: RefCell::new(None),
        confirm_text,
        confirm,
        cancel,
        inputs: RefCell::new(inputs),
        outputs: RefCell::new(outputs),
        input_index: Cell::new(input_index),
        output_index: Cell::new(output_index),
        visible: Cell::new(false),
    }
}

/// A label + hint on the left, a control on the right, with the padding of a settings row.
fn settings_row<T>(label: &str, hint: Option<&str>, control: &Ref<T>) -> Ref<Border>
where
    T: ferroui_base::ObjectType + ferroui_base::Upcast<Control>,
{
    let text_column = StackPanel::new();
    text_column.set_spacing(2.0);
    text_column.children().add(ui_kit::body(label, 13.5));
    if let Some(hint) = hint {
        text_column.children().add(ui_kit::muted(hint, 11.5));
    }
    let row = Grid::new();
    row.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&text_column, 0);
    let control: Ref<Control> = control.clone().upcast::<Control>();
    Grid::set_column(&control, 1);
    row.children().add(&text_column);
    row.children().add(control);
    let border = Border::new();
    border.set_padding(Thickness {
        left: 14.0,
        top: 11.0,
        right: 14.0,
        bottom: 11.0,
    });
    border.set_child(&row);
    border
}

/// One settings page: a headline, a one-line explanation and a card of rows.
fn settings_section(
    glyph: icons::Icon,
    title: &str,
    subtitle: &str,
    rows: Vec<Ref<Border>>,
) -> Ref<StackPanel> {
    let heading = ui_kit::row(8.0);
    heading.children().add(icons::icon(glyph, 16.0));
    heading.children().add(ui_kit::headline(title));
    let column = StackPanel::new();
    column.set_spacing(2.0);
    column.children().add(&heading);
    column.children().add(ui_kit::muted(subtitle, 12.0));

    let rows_panel = StackPanel::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            rows_panel.children().add(ui_kit::divider());
        }
        rows_panel.children().add(row);
    }
    let card = ui_kit::card(&rows_panel, 0.0);

    let section = StackPanel::new();
    section.set_spacing(10.0);
    section.children().add(&column);
    section.children().add(&card);
    section
}
