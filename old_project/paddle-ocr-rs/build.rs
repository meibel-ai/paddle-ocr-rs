fn main() {
    #[cfg(target_os = "linux")]
    {
        let compat_c = std::path::Path::new("glibc_compat.c");
        if compat_c.exists() {
            cc::Build::new()
                .file(compat_c)
                .compile("glibc_compat");
            println!("cargo:rerun-if-changed=glibc_compat.c");
        }
    }
}
