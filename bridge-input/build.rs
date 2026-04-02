fn main() {
    #[cfg(target_os = "macos")]
    {
        // Link macOS frameworks
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=IOKit");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
    }

    #[cfg(target_os = "linux")]
    {
        // Link X11 for input injection via XTest extension
        println!("cargo:rustc-link-lib=x11");
        println!("cargo:rustc-link-lib=xtst");
    }
}
