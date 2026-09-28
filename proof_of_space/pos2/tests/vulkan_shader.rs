#[test]
fn portable_hash_shader_validates_without_gpu() {
    let source = concat!(
        include_str!("../src/vulkan_aes.wgsl"),
        "\n",
        include_str!("../src/vulkan.wgsl")
    );
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("PoS2 shader must use portable WGSL without optional capabilities");
}

#[test]
fn resident_k28_shader_validates_without_gpu() {
    let source = concat!(
        include_str!("../src/vulkan_aes.wgsl"),
        "\n",
        include_str!("../src/vulkan_compact.wgsl")
    );
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("resident k28 shader must use portable WGSL");
}

#[test]
fn resident_radix_shader_validates_with_subgroups() {
    let source = include_str!("../src/vulkan_radix.wgsl");
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::SUBGROUP | naga::valid::Capabilities::SUBGROUP_BARRIER,
    )
    .validate(&module)
    .expect("resident radix shader must validate with checked subgroup capabilities");
}

#[test]
fn resident_packing_shader_validates_without_optional_capabilities() {
    let source = include_str!("../src/vulkan_packing.wgsl");
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("resident packing shader must use portable WGSL");
}
