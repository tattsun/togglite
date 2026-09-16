fn main() {
    println!("cargo:rerun-if-changed=togglite.rc");
    println!("cargo:rerun-if-changed=togglite.manifest");
    println!("cargo:rerun-if-changed=icons/idle.ico");
    println!("cargo:rerun-if-changed=icons/running.ico");
    embed_resource::compile("togglite.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("failed to compile resources");
}
