fn main() {
    cc::Build::new().file("HPatch/patch.c").compile("hpatch");
}
