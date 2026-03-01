fn main() {
    // Set @rpath for libswift_Concurrency.dylib used by screencapturekit crate.
    // Use /usr/lib/swift/ (dyld shared cache) to avoid loading duplicates from CommandLineTools.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
