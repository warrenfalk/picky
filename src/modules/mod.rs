pub mod applications;
pub mod chrome_tabs;
pub mod firefox_tabs;
pub mod niri_windows;

use crate::module::Module;

pub fn default_modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(applications::ApplicationsModule::new()),
        Box::new(firefox_tabs::FirefoxTabsModule::new()),
        Box::new(chrome_tabs::ChromeTabsModule::new()),
        Box::new(niri_windows::NiriWindowsModule::new()),
    ]
}
