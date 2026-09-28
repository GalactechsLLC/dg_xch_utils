struct PackingConfiguration {
    count: u32,
    first_chunk: u32,
    descriptor_count: u32,
    output_words: u32,
}

struct PackingErrors {
    flags: atomic<u32>,
}

@group(0) @binding(0) var<uniform> configuration: PackingConfiguration;
@group(0) @binding(1) var<storage, read> input_first: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> input_second: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> input_third: array<vec4<u32>>;
@group(0) @binding(4) var<storage, read> input_fourth: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read> input_fifth: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read_write> boundaries: array<u32>;
@group(0) @binding(7) var<storage, read> descriptors: array<vec4<u32>>;
@group(0) @binding(8) var<storage, read_write> packed_output: array<u32>;
@group(0) @binding(9) var<storage, read_write> errors: PackingErrors;

fn load_fragment(position: u32) -> vec2<u32> {
    if position >= configuration.count {
        atomicOr(&errors.flags, 4u);
        return vec2<u32>(0u);
    }
    let offset = position & 0x03ffffffu;
    switch position >> 26u {
        case 0u: {
            if offset < arrayLength(&input_first) { return input_first[offset].xy; }
        }
        case 1u: {
            if offset < arrayLength(&input_second) { return input_second[offset].xy; }
        }
        case 2u: {
            if offset < arrayLength(&input_third) { return input_third[offset].xy; }
        }
        case 3u: {
            if offset < arrayLength(&input_fourth) { return input_fourth[offset].xy; }
        }
        case 4u: {
            if offset < arrayLength(&input_fifth) { return input_fifth[offset].xy; }
        }
        default: {}
    }
    atomicOr(&errors.flags, 4u);
    return vec2<u32>(0u);
}

fn fragment_delta(position: u32, first: u32, chunk: u32) -> vec2<u32> {
    let current = load_fragment(position);
    var previous = vec2<u32>(0u, chunk << 12u);
    if position != first {
        previous = load_fragment(position - 1u);
    }
    if current.y >> 12u != chunk
        || current.y < previous.y
        || (current.y == previous.y && current.x < previous.x)
    {
        atomicOr(&errors.flags, 1u);
        return vec2<u32>(0u);
    }
    let difference = vec2<u32>(
        current.x - previous.x,
        current.y - previous.y - u32(current.x < previous.x),
    );
    if difference.y > 3u {
        atomicOr(&errors.flags, 2u);
        return vec2<u32>(0u);
    }
    return difference;
}

@compute @workgroup_size(256)
fn packing_boundaries(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let chunk = invocation.x;
    if chunk > 4096u { return; }
    if chunk >= arrayLength(&boundaries) {
        atomicOr(&errors.flags, 4u);
        return;
    }
    let wanted_high = chunk << 12u;
    var lower = 0u;
    var upper = configuration.count;
    while lower < upper {
        let middle = lower + (upper - lower) / 2u;
        if load_fragment(middle).y < wanted_high {
            lower = middle + 1u;
        } else {
            upper = middle;
        }
    }
    boundaries[chunk] = lower;
}

@compute @workgroup_size(256)
fn packing_chunks(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let descriptor_index = group.y;
    if descriptor_index >= configuration.descriptor_count { return; }
    if descriptor_index >= arrayLength(&descriptors)
        || configuration.first_chunk >= 4096u
        || descriptor_index >= 4096u - configuration.first_chunk
    {
        atomicOr(&errors.flags, 4u);
        return;
    }
    let descriptor = descriptors[descriptor_index];
    let first = descriptor.x;
    let values = descriptor.y;
    if values > 1048576u || first > configuration.count {
        atomicOr(&errors.flags, 4u);
        return;
    }
    if values > configuration.count - first {
        atomicOr(&errors.flags, 4u);
        return;
    }
    let delta_words = (values + 3u) / 4u;
    let stub_words = (values * 26u + 31u) / 32u;
    let total_words = delta_words + stub_words;
    if descriptor.z > configuration.output_words {
        atomicOr(&errors.flags, 4u);
        return;
    }
    if total_words > configuration.output_words - descriptor.z
        || descriptor.w != descriptor.z + delta_words
        || configuration.output_words > arrayLength(&packed_output)
    {
        atomicOr(&errors.flags, 4u);
        return;
    }
    let word_index = group.x * 256u + local;
    if word_index >= total_words { return; }
    let chunk = configuration.first_chunk + descriptor_index;
    var packed = 0u;
    if word_index < delta_words {
        for (var lane = 0u; lane < 4u; lane += 1u) {
            let relative = word_index * 4u + lane;
            if relative < values {
                let difference = fragment_delta(first + relative, first, chunk);
                packed |= ((difference.x >> 26u) | (difference.y << 6u)) << (lane * 8u);
            }
        }
    } else {
        let first_bit = (word_index - delta_words) * 32u;
        var relative = first_bit / 26u;
        let skipped = first_bit % 26u;
        if relative < values {
            packed = (fragment_delta(first + relative, first, chunk).x & 0x03ffffffu) >> skipped;
        }
        relative += 1u;
        var pending = 26u - skipped;
        while pending < 32u && relative < values {
            packed |= (fragment_delta(first + relative, first, chunk).x & 0x03ffffffu) << pending;
            pending += 26u;
            relative += 1u;
        }
    }
    packed_output[descriptor.z + word_index] = packed;
}
