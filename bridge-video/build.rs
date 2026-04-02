fn main() {
    #[cfg(target_os = "macos")]
    {
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

    #[cfg(target_os = "linux")]
    {
        // Compile NVIDIA V4L2 encoder helper
        cc::Build::new()
            .file("src/linux/nvenc_helper.c")
            .include("/usr/src/jetson_multimedia_api/include")
            .warnings(false)
            .compile("nvenc_helper");

        // Link NVIDIA V4L2 wrapper (provides v4l2_open/v4l2_ioctl/v4l2_mmap)
        println!("cargo:rustc-link-search=native=/usr/lib/aarch64-linux-gnu/tegra");
        println!("cargo:rustc-link-lib=dylib=nvv4l2");

        // Link X11/XCB libraries for screen capture
        println!("cargo:rustc-link-lib=X11");
        println!("cargo:rustc-link-lib=xcb");
        println!("cargo:rustc-link-lib=xcb-shm");
    }
}
