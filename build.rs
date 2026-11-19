fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_manifest_file("coretuner.manifest");
        if std::path::Path::new("assets/icon.ico").exists() {
            res.set_icon("assets/icon.ico");
        }
        res.set("ProductName", "CoreTuner");
        res.set("FileDescription", "CoreTuner — CPU affinity & priority manager");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres failed: {e}");
        }
    }
}
