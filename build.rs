use std::{env, path::PathBuf};

fn main() {
    // 動的ライブラリの探索パス & リンク
    println!("cargo:rustc-link-search=native=vendor/macos");
    println!("cargo:rustc-link-lib=dylib=omt");
    println!("cargo:rustc-link-lib=dylib=vmx");

    // libomt.h からバインディング生成
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindgen::Builder::default()
        .header("vendor/include/libomt.h")
        // 生成に不要な警告を抑止したい場合は .clang_arg などを適宜
        .generate()
        .expect("bindgen failed")
        .write_to_file(out.join("bindings.rs"))
        .expect("write bindings.rs failed");
}
