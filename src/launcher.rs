use relm4::RelmApp;

pub fn run() {
    let app = RelmApp::new("com.warren.picky");
    app.run::<crate::app::PickerApp>(());
}
