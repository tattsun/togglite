fn main() {
    println!("cargo:rerun-if-changed=togglite.rc");
    println!("cargo:rerun-if-changed=togglite.manifest");
    println!("cargo:rerun-if-changed=icons/idle.ico");
    println!("cargo:rerun-if-changed=icons/running.ico");

    // Feed the crate version into the VERSIONINFO resource so it stays in sync with Cargo.toml.
    let ver = env!("CARGO_PKG_VERSION");
    let comma = format!(
        "{},{},{},0",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    );
    let macros = [format!("VER_COMMA={comma}"), format!("VER_STR=\"{ver}\"")];
    embed_resource::compile("togglite.rc", &macros)
        .manifest_optional()
        .expect("failed to compile resources");
}
