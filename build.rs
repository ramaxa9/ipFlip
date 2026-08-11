#[cfg(windows)]
use std::fs::File;

#[cfg(windows)]
use std::path::{Path, PathBuf};

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
fn convert_png_to_ico(png_path: &Path) -> Option<PathBuf> {
  let image = image::open(png_path).ok()?.to_rgba8();
  let (width, height) = image.dimensions();

  let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
  let icon_image = ico::IconImage::from_rgba_data(width, height, image.into_raw());
  let entry = ico::IconDirEntry::encode(&icon_image).ok()?;
  icon_dir.add_entry(entry);

  let out_dir = PathBuf::from(std::env::var("OUT_DIR").ok()?);
  let generated_path = out_dir.join("generated_icon.ico");
  let mut file = File::create(&generated_path).ok()?;
  icon_dir.write(&mut file).ok()?;

  Some(generated_path)
}

#[cfg(windows)]
fn resolve_icon_path() -> Option<String> {
  let ico_candidates = ["icon.ico", "../icon.ico"];
  if let Some(icon_path) = ico_candidates.iter().find(|p| Path::new(*p).exists()) {
    return Some((*icon_path).to_string());
  }

  let png_candidates = ["icon.png", "../icon.png"];
  for png_candidate in png_candidates {
    let path = Path::new(png_candidate);
    if !path.exists() {
      continue;
    }

    if let Some(generated_path) = convert_png_to_ico(path) {
      println!(
        "cargo:warning=Generated icon from {} at {}",
        path.display(),
        generated_path.display()
      );
      return Some(generated_path.to_string_lossy().to_string());
    }
  }

  None
}

#[cfg(windows)]
fn main() {
  println!("cargo:rerun-if-changed=manifest.xml");
  println!("cargo:rerun-if-changed=icon.ico");
  println!("cargo:rerun-if-changed=../icon.ico");
  println!("cargo:rerun-if-changed=icon.png");
  println!("cargo:rerun-if-changed=../icon.png");

    let mut resource = winresource::WindowsResource::new();
    resource.set_manifest(&manifest_xml());
    resource.set("FileDescription", "ipFlip Rust");
    resource.set("ProductName", "ipFlip");
    resource.set("OriginalFilename", "rust_ipflip.exe");

  if let Some(icon_path) = resolve_icon_path() {
    resource.set_icon(&icon_path);
    } else {
    println!(
      "cargo:warning=No icon resource found. Provide icon.ico or icon.png in rust_ipflip/ or parent folder."
    );
    }

    if let Err(err) = resource.compile() {
        panic!("Failed to compile Windows resources: {err}");
    }
}

#[cfg(not(windows))]
fn main() {}
