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
        // uinput uses kernel ioctls directly, no extra libraries needed
    }
}
