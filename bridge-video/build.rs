fn main() {
    // Link macOS frameworks
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=VideoToolbox");
    println!("cargo:rustc-link-lib=framework=IOSurface");
    println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=QuartzCore");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");

    // Add Swift runtime library path for screencapturekit crate
    // (it uses Swift internally and needs libswift_Concurrency.dylib at runtime)
    // Use /usr/lib/swift/ (dyld shared cache) to avoid loading duplicates from CommandLineTools.
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
