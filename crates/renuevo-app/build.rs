fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // `scripts/bundle-macos.sh` cambia en el binario la ruta de libheif por la copia que mete en
    // Renuevo.app; deja sitio en la cabecera Mach-O para poder reescribirla.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-headerpad_max_install_names");
    }
}
