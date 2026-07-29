fn main() {
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("assets/Ext2Mgr.ico");
    resource.set_manifest_file("app.manifest");
    if let Err(err) = resource.compile() {
        println!("cargo:warning=winres failed: {err}");
    }
}
