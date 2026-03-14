# Iced Migration Lessons Learned

## Initial Notes

- The repo did not already have Iced in `Cargo.lock` or in the local cargo source cache, so the migration had to be validated against current upstream Iced documentation and then corrected by real compilation. `iced-best-practices.md` was directionally useful, but it did not mention this very practical “expect API drift and compile to truth” step.
- The existing Relm4 UI relied on GTK icon-theme lookup for many application icons. Iced has straightforward image-file support, but that is not the same capability. `iced-best-practices.md` does not call out asset and icon-theme migration as a real UI-porting concern when leaving a toolkit with desktop-theme integration.
- The old launcher sized itself from the realized GTK window’s current monitor. In Iced, that exact monitor API path is not available in the same way, so the migration needs an alternate sizing source. Here the fallback is Niri’s own JSON output metadata, which is compositor-specific. The guide does not mention window-management and monitor-query gaps that can appear when leaving GTK.

## API Corrections From Real Compilation

- The first pass assumed an older-looking builder call shape, `iced::application("title", update, view).run_with(...)`. In the installed `iced 0.14.0`, `application(...)` takes the boot function first and the builder runs with `.run()`. This is exactly the kind of version-specific trap that should be called out more concretely in `iced-best-practices.md`.
- The first pass also assumed convenience helpers like `keyboard::on_key_press` and `text_input::focus`. In the installed crate, the working APIs are `event::listen_with(...)` for filtered key handling and `iced::widget::operation::{focus, focus_next, focus_previous}` for focus changes. The guide warns that helper names drift, but it would be stronger if it explicitly advised checking the installed crate source for input and focus helpers before wiring keyboard-heavy UIs.
- Widget IDs are generic `iced::widget::Id` values, not widget-specific `text_input::Id` values in this version. That matters for any migration that depends on programmatic focus.
- The builder also rejected an inline `.theme(|_| Theme::Dark)` closure because the trait bound in this version expects a more general function shape. Replacing it with a named `fn theme(&PickerApp) -> Theme` solved it. That is a minor issue, but it is another example of why compilation against the installed crate matters more than relying on broad architectural guidance alone.
