fn main() {
    println!("cargo:rerun-if-env-changed=DECTALK_LIB_DIR");
    if std::env::var("CARGO_FEATURE_DECTALK").is_ok() {
        let dir = std::env::var("DECTALK_LIB_DIR").expect(
            "the dectalk feature needs DECTALK_LIB_DIR, the directory holding libdectalk.a",
        );
        println!("cargo:rustc-link-search=native={}", dir);
        println!("cargo:rustc-link-lib=static=dectalk");
    }
}
