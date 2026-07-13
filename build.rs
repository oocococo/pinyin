fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => build_macos(),
        Ok("windows") => build_windows(),
        _ => {}
    }
}

fn build_macos() {
    println!("cargo:rerun-if-changed=src/mac/native.mm");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=ApplicationServices");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=Foundation");

    cc::Build::new()
        .cpp(true)
        .flag("-std=c++17")
        .file("src/mac/native.mm")
        .compile("pal_pinyin_mac");
}

fn build_windows() {
    println!("cargo:rerun-if-changed=src/win/native.cpp");

    let mut build = cc::Build::new();
    build.cpp(true).file("src/win/native.cpp");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/std:c++17");
    } else {
        build.flag("-std=c++17");
    }

    build.compile("pal_pinyin_win");
    println!("cargo:rustc-link-lib=dylib=user32");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
