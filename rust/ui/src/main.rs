mod app;
mod art;
mod mod_view;
mod trainer;

fn main() -> Result<(), slint::PlatformError> {
    if std::env::args().any(|argument| argument == "--self-check") {
        std::process::exit(app::self_check());
    }
    if std::env::args().any(|argument| argument == "--trainer-self-test") {
        std::process::exit(app::trainer_self_test());
    }
    app::run()
}
