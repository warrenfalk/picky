pub mod applications;
pub mod codex_sessions;
pub mod mako_notifications;
pub mod niri_windows;
pub mod niri_workspaces;

use crate::module::Module;

pub fn default_modules(codex_sessions: codex_sessions::SessionStore) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(mako_notifications::MakoNotificationsModule::new()),
        Box::new(codex_sessions::CodexSessionsModule::new(codex_sessions)),
        Box::new(applications::ApplicationsModule::new()),
        Box::new(niri_windows::NiriWindowsModule::new()),
        Box::new(niri_workspaces::NiriWorkspacesModule::new()),
    ]
}
