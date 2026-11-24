//! M0 FerroUI Android gate: a shared library with the same 5,000-message list and multiline
//! input as the desktop spike, packaged as an APK by the thin Java layer of `ferroui-android`.

#![cfg(target_os = "android")]

ferroui_android::android_application!(app::build);

mod app {
    use ferroui_android::{AndroidApplicationExtensions, AndroidPlatformOptions};
    use ferroui_base::collections::FerroList;
    use ferroui_base::layout::{HorizontalAlignment, VerticalAlignment};
    use ferroui_base::{FerroObjectImpl, Ref, ferro_class, ferro_impl_classes, instantiate};
    use ferroui_controls::{
        AppBuilder, Application, ApplicationImpl, ApplicationImplExt, Control, ItemsSource, ListBox,
        NewApplication, StackPanel, TextBlock, TextBox,
    };
    use ferroui_themes_material::{MaterialAppBuilderExtension, MaterialTheme};
    use std::cell::Cell;
    use std::rc::Rc;

    const MESSAGE_COUNT: usize = 5_000;

    #[repr(C)]
    pub struct App {
        base: Application,
    }

    ferro_class!(App: Application);
    ferro_impl_classes!(App: FerroObjectImpl);

    impl NewApplication for App {
        fn new_application() -> Ref<Self> {
            instantiate(Self {
                base: Application::construct(),
            })
        }
    }

    impl ApplicationImpl for App {
        fn initialize(this: &Self) {
            Self::parent_initialize(this);
            this.set_name(Some("FerroUI Android spike".to_string()));
            this.styles().add(MaterialTheme::new().as_style());
        }

        fn on_framework_initialization_completed(this: &Self) {
            let lifetime = this.application_lifetime();
            if let Some(lifetime) = lifetime
                .as_ref()
                .and_then(|lifetime| lifetime.as_activity_application_lifetime())
            {
                lifetime.set_main_view_factory(Some(Rc::new(create_main_view)));
            }
            Self::parent_on_framework_initialization_completed(this);
        }
    }

    /// The application builder handed to `android_application!`.
    pub fn build() -> AppBuilder {
        AppBuilder::configure::<App>()
            .with(Rc::new(AndroidPlatformOptions::default()))
            .use_android()
            .with_material_fonts()
    }

    fn create_main_view() -> Ref<Control> {
        let message_count = std::env::var("FERROUI_MESSAGES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(MESSAGE_COUNT);

        let header = TextBlock::new();
        header.set_text(Some("FerroUI M0 gate — Android"));
        header.set_font_size(20.0);

        let messages = Rc::new(FerroList::new());
        messages.add_range((0..message_count).map(|index| {
            format!("#{index:05}  alice  ·  a message long enough to exercise text layout and wrapping")
        }));

        let list = ListBox::new();
        list.set_items_source(Some(ItemsSource::from(messages.clone())));
        list.set_height(550.0);

        let input = TextBox::new();
        input.set_accepts_return(true);
        input.set_height(96.0);
        input.set_text(Some("Привет! 你好，世界。 emoji 🎉"));

        let status = TextBlock::new();
        let initial_status = format!("{message_count} messages");
        status.set_text(Some(&initial_status));

        let next_index = Rc::new(Cell::new(message_count));
        let append = ferroui_controls::Button::new();
        append.set_content(Some(Rc::new("Append".to_string())));
        append.click({
            let messages = messages.clone();
            let status = status.clone();
            let next_index = next_index.clone();
            move |_, _| {
                let index = next_index.get();
                next_index.set(index + 1);
                messages.insert(0, format!("#{index:05}  bob  ·  appended on the phone"));
                let updated = format!("{} messages", index + 1);
                status.set_text(Some(&updated));
            }
        });

        let panel = StackPanel::new();
        panel.set_spacing(12.0);
        panel.set_horizontal_alignment(HorizontalAlignment::Stretch);
        panel.set_vertical_alignment(VerticalAlignment::Stretch);
        panel.children().add(header);
        panel.children().add(list);
        panel.children().add(input);
        panel.children().add(append);
        panel.children().add(status);

        panel.upcast()
    }
}
