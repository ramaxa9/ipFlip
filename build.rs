#[cfg(windows)]
fn requested_execution_level() -> &'static str {
    match std::env::var("PROFILE").as_deref() {
        Ok("release") => "requireAdministrator",
        _ => "asInvoker",
    }
}

#[cfg(windows)]
fn manifest_xml() -> String {
    include_str!("manifest.xml").replace("requireAdministrator", requested_execution_level())
}

#[cfg(windows)]
fn main() {
  println!("cargo:rerun-if-changed=manifest.xml");
  println!("cargo:rerun-if-changed=icon.ico");

    let mut resource = winresource::WindowsResource::new();
    resource.set_manifest(&manifest_xml());
    resource.set("FileDescription", "ipFlip Rust");
    resource.set("ProductName", "ipFlip");
    resource.set("OriginalFilename", "rust_ipflip.exe");
    resource.set_icon("icon.ico");

    if let Err(err) = resource.compile() {
        panic!("Failed to compile Windows resources: {err}");
    }
}

#[cfg(not(windows))]
fn main() {}
