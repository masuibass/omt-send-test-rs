// build.rs が OUT_DIR に生成したものを読込
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
