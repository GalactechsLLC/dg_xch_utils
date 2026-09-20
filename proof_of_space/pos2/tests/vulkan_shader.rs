#[test]
fn portable_hash_shader_validates_without_gpu() {
    let source = include_str!("../src/vulkan.wgsl");
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("PoS2 shader must use portable WGSL without optional capabilities");
}
