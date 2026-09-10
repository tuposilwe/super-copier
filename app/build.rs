fn main() {
    // Embeds `assets/icon.ico` as the .exe's file icon (shown in Explorer).
    // A no-op on non-Windows targets.
    if let Err(e) = embed_resource::compile("assets/icon.rc", embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=failed to embed Windows icon resource: {e:?}");
    }
}
