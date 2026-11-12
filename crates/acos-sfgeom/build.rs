// Compiles the vendored AOSP geometry kernel (Android 14, libui Transform/Rect plus the
// RequestedLayerState / LayerSnapshotBuilder bodies in cpp/sfgeom_kernel.cpp).
//
// Transform.cpp carries an unused `Transform::transform(const Region&)` overload. We do not
// vendor Region.cpp (its ui/RegionHelper.h is absent from the AOSP checkout and fetching it was
// not approved), so that function's Region symbols are unresolved. -ffunction-sections plus
// --gc-sections drops the unused section before the final link, exactly as SPIKE-B did.

use std::path::Path;

fn main() {
    let aosp = Path::new("third_party/aosp/libs/ui");
    let aosp_include = aosp.join("include");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .opt_level(2)
        .include(aosp_include)
        .include("third_party/stubs")
        .include("cpp")
        .file(aosp.join("Rect.cpp"))
        .file(aosp.join("Transform.cpp"))
        .file("cpp/sfgeom_kernel.cpp")
        .flag_if_supported("-ffunction-sections")
        .flag_if_supported("-fdata-sections")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("acos_sfgeom");

    println!("cargo:rustc-link-arg=-Wl,--gc-sections");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp/sfgeom_kernel.cpp");
    println!("cargo:rerun-if-changed=cpp/sfgeom_kernel.h");
    println!("cargo:rerun-if-changed=third_party/aosp");
    println!("cargo:rerun-if-changed=third_party/stubs");
}
