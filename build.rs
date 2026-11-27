fn main() {
    // Icône de l'exécutable Windows (ignoré sur les autres plateformes).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let _ = embed_resource::compile("assets/ferrox.rc", embed_resource::NONE);
    }
    println!("cargo:rerun-if-changed=assets/ferrox.rc");
    println!("cargo:rerun-if-changed=assets/ferrox.ico");
}
