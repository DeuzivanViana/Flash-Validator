//! Embeds the icon and version information into the Windows executable.

include!("../../assets/icon_art.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon_art.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("flashvalidator.ico");
    std::fs::write(&ico, icon_ico(&[16, 24, 32, 48, 64, 256])).expect("write icon");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy())
        .set("ProductName", "FlashValidator")
        .set("FileDescription", "FlashValidator")
        .set("InternalName", "flashvalidator-gui")
        .set("OriginalFilename", "flashvalidator-gui.exe")
        .set("CompanyName", "FlashValidator contributors")
        .set("LegalCopyright", "Copyright (c) 2026 FlashValidator contributors. MIT OR Apache-2.0.");
    if let Err(e) = res.compile() {
        println!("cargo:warning=Windows resources (icon, version info) were not embedded: {e}");
    }
}
