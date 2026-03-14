# Iced Migration Checklist

- [x] Replace the Relm4 launcher and root component with an Iced application builder entrypoint.
- [x] Keep the module/domain layer intact and move search/activation into Iced `Task`s.
- [x] Rebuild picker behavior in Iced: query updates, selection, action shortcuts, activation, refresh, and close.
- [x] Preserve undecorated centered window sizing, including monitor-height sizing based on the focused Niri output.
- [ ] Verify runtime behavior manually in the compositor and fix any interaction regressions found after the port.
- [ ] Decide whether GTK icon-theme name resolution needs to be reintroduced or whether file-path icons plus symbolic fallbacks are sufficient in the Iced version.
