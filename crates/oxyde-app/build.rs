//! On Windows, gives `oxyde.exe` its icon and version information (what Explorer shows),
//! from `packaging/oxyde.ico`. Needs the Windows SDK's resource compiler, which comes with
//! Visual Studio's C++ workload.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/oxyde.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../packaging/oxyde.ico")
        .set("ProductName", "Oxyde")
        .set("FileDescription", "Oxyde — parametric CAD")
        .set("CompanyName", "TheApache142857")
        .set("LegalCopyright", "MIT License");
    if let Err(e) = resource.compile() {
        // A missing resource compiler costs the icon, not the build.
        println!("cargo:warning=no icon in oxyde.exe: {e}");
    }
}
