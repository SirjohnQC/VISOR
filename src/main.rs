use visor::config::Config;

fn main() {
    let path = Config::default_path();
    Config::write_defaults_if_missing(&path);

    // Load once with defaults so we know what level to log at, then re-read
    // through the logger so parse failures are actually recorded.
    let bootstrap = Config::load_or_default(&path);
    let _guard = match visor::logging::init(&bootstrap.log.level) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("visor: could not initialise logging: {e}");
            return;
        }
    };

    let cfg = Config::load_or_default(&path);
    tracing::info!(?cfg, "VISOR starting");
    tracing::info!("skeleton only — no engine yet");
}
