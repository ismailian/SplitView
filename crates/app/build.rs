//! Windows: embeds the icon and version info into splitview.exe (Explorer, taskbar, installer).

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/splitview.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/splitview.ico")
            .set("ProductName", "SplitView")
            .set("FileDescription", "SplitView")
            .set("CompanyName", "SplitView")
            .set("OriginalFilename", "splitview.exe");
        res.compile()
            .expect("embedding the Windows icon needs rc.exe from the Windows SDK");
    }
}
