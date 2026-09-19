mod app;
mod fuzzy;
mod launcher;
mod module;
mod modules;

fn main() -> anyhow::Result<()> {
    launcher::run()
}
