pub mod applications;
pub mod mako_notifications;
pub mod niri_windows;
pub mod niri_workspaces;

use crate::module::Module;

pub fn default_modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(mako_notifications::MakoNotificationsModule::new()),
        Box::new(applications::ApplicationsModule::new()),
        Box::new(niri_windows::NiriWindowsModule::new()),
        Box::new(niri_workspaces::NiriWorkspacesModule::new()),
    ]
}
