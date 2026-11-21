// Node resolves the Node-API functions of the addon when it loads it.
// With `dynamic_lookup`, ld 27037 writes the dylib without chained fixups
// unless asked.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
        println!("cargo:rustc-cdylib-link-arg=-Wl,-fixup_chains");
    }
}
