use std::env;

fn main() {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if arch == "riscv32" {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    } else if arch == "xtensa" {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }
}
