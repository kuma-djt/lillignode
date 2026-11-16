//! Builds the icon into lillignode.exe on Windows. Nothing happens on other
//! systems: there the program has no icon of its own.
//!
//! Building the icon in needs a resource compiler (rc.exe, from the Windows
//! SDK). Without one an ordinary build still succeeds, with a warning and no
//! icon. The packaging script sets LILLIGNODE_REQUIRE_ICON, and then a
//! missing icon stops the build: a release is never made without it.

fn main() {
    println!("cargo:rerun-if-changed=assets/lillignode.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LILLIGNODE_REQUIRE_ICON");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/lillignode.ico");
        if let Err(e) = resource.compile() {
            if std::env::var_os("LILLIGNODE_REQUIRE_ICON").is_some() {
                panic!("could not build the icon into lillignode.exe: {e}");
            }
            println!("cargo:warning=lillignode.exe is built without its icon: {e}");
        }
    }
}
