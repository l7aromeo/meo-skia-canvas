fn main() {
    // On Linux, compile the BDF stub to satisfy fontconfig's
    // FT_Get_BDF_Property calls. Skia embeds its own FreeType without BDF
    // support, and linking a full FreeType with BDF causes duplicate symbol
    // conflicts. This stub returns "not found" for BDF queries - we don't
    // use legacy bitmap fonts anyway.
    #[cfg(target_os = "linux")]
    {
        cc::Build::new().file("src/bdf_stub.c").compile("bdf_stub");
    }
}
