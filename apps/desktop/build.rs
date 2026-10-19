fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    for file in [
        "assets/app.rc",
        "assets/icon.ico",
        "assets/tray-connected.ico",
        "assets/tray-disconnected.ico",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    // Shell loads these IDs from the executable, so missing resources are a
    // build error rather than an application that starts without a tray icon.
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("compile Omdrop Windows icon resources");
}
