fn main() {
    cc::Build::new()
        .static_crt(true)
        .file("HPatch/patch.c")
        .compile("hpatch");
}
