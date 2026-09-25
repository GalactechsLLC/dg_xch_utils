#ifdef __CUDACC__
typedef unsigned int uint;
typedef unsigned long long Address;
struct KernelParameters { Address pointers[12]; uint words[32]; };
typedef const KernelParameters& Config;
#define DEVICE __device__
#define OUTPUT(type, name, size) type* name
#define INPUT(type, name, size) const type* name
#define REF(type, name) type& name
#define BARRIER() __syncthreads()
#define UNROLL _Pragma("unroll")
DEVICE uint read_word(Address address, uint index) { return ((uint*)address)[index]; }
DEVICE Address read_pair(Address address, uint index) { return ((const Address*)address)[index]; }
DEVICE void read_quad(Address address, uint index, OUTPUT(uint, words, 4)) { uint4 value = ((const uint4*)address)[index]; words[0] = value.x; words[1] = value.y; words[2] = value.z; words[3] = value.w; }
DEVICE void write_word(Address address, uint index, uint value) { ((uint*)address)[index] = value; }
DEVICE void write_pair(Address address, uint index, uint first, uint second) { ((Address*)address)[index] = Address(first) | (Address(second) << 32u); }
DEVICE void write_quad(Address address, uint index, uint first, uint second, uint third, uint fourth) { ((uint4*)address)[index] = make_uint4(first, second, third, fourth); }
DEVICE uint add_word(Address address, uint index, uint value) { return atomicAdd(((uint*)address) + index, value); }
DEVICE uint exchange_word(Address address, uint index, uint value) { return atomicExch(((uint*)address) + index, value); }
DEVICE uint group_width() { return blockDim.x; }
DEVICE uint reserve_output(Address address) {
    uint mask = __activemask();
    uint leader = uint(__ffs(mask) - 1);
    uint lane = threadIdx.x & 31u;
    uint base = 0u;
    if (lane == leader) base = add_word(address, 0u, uint(__popc(mask)));
    base = __shfl_sync(mask, base, leader);
    return base + uint(__popc(mask & ((1u << lane) - 1u)));
}
#else
#extension GL_EXT_buffer_reference : require
#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require
#extension GL_KHR_shader_subgroup_basic : require
#extension GL_KHR_shader_subgroup_ballot : require
#extension GL_KHR_shader_subgroup_arithmetic : require
#extension GL_EXT_control_flow_attributes : require
#define Address uint64_t
#define Config Address
#define DEVICE
#define OUTPUT(type, name, size) inout type name[size]
#define INPUT(type, name, size) in type name[size]
#define REF(type, name) inout type name
#define BARRIER() barrier()
#define UNROLL [[unroll]]
layout(constant_id = 0) const uint operation_kind = 0u;
layout(local_size_x_id = 1) in;
layout(push_constant) uniform Push { uint parameters[32]; } push;
uint group_width() { return gl_WorkGroupSize.x; }
layout(buffer_reference, std430, buffer_reference_align = 4) buffer Words { uint data[]; };
layout(buffer_reference, std430, buffer_reference_align = 8) buffer Pairs { uvec2 data[]; };
layout(buffer_reference, std430, buffer_reference_align = 16) buffer Quads { uvec4 data[]; };
uint read_word(Address address, uint index) { return Words(address).data[index]; }
Address read_pair(Address address, uint index) { uvec2 value = Pairs(address).data[index]; return Address(value.x) | (Address(value.y) << 32u); }
void read_quad(Address address, uint index, OUTPUT(uint, words, 4)) { uvec4 value = Quads(address).data[index]; words[0] = value.x; words[1] = value.y; words[2] = value.z; words[3] = value.w; }
void write_word(Address address, uint index, uint value) { Words(address).data[index] = value; }
void write_pair(Address address, uint index, uint first, uint second) { Pairs(address).data[index] = uvec2(first, second); }
void write_quad(Address address, uint index, uint first, uint second, uint third, uint fourth) { Quads(address).data[index] = uvec4(first, second, third, fourth); }
uint add_word(Address address, uint index, uint value) { return atomicAdd(Words(address).data[index], value); }
uint exchange_word(Address address, uint index, uint value) { return atomicExchange(Words(address).data[index], value); }
uint reserve_output(Address address) {
    uvec4 mask = subgroupBallot(true);
    uint base = 0u;
    if (subgroupElect()) base = add_word(address, 0u, subgroupBallotBitCount(mask));
    return subgroupBroadcastFirst(base) + subgroupBallotExclusiveBitCount(mask);
}
shared uint left_entries[304];
shared uint left_values[304];
shared uint right_entries[304];
shared uint right_values[304];
shared uint generation_words[512];
shared uint first_partition_records[1024];
shared uint first_partition_counts[64];
shared uint first_partition_bases[64];
shared uint local_counts[1024];
shared uint second_partition_counts[64];
shared uint second_partition_bases[64];
shared uint second_partition_records[3072];
shared uint coarse_counts[576];
shared uint coarse_bases[576];
shared uint coarse_records[10368];
shared uint tile_counts[512];
shared uint tile_prefix[128];
shared uint tile_records[8192];
shared uint tile_error;
shared uint tile_job_base;
shared uint hash_heads[512];
shared uint right_present[473];
shared uint matches[512];
shared uint match_count;
shared uint output_base;
shared uint left_count;
shared uint right_count;
#endif

DEVICE Address pointer_at(Config config, uint slot) {
#ifdef __CUDACC__
    return config.pointers[slot];
#else
    uint packed_slot = operation_kind == 1u || operation_kind == 18u || operation_kind == 21u ? (slot == 1u ? 0u : (slot == 3u ? 1u : (slot == 5u ? 2u : 3u))) : slot;
    return Address(push.parameters[packed_slot * 2u]) | (Address(push.parameters[packed_slot * 2u + 1u]) << 32u);
#endif
}
DEVICE uint parameter(Config config, uint slot) {
#ifdef __CUDACC__
    return config.words[slot];
#else
    if (operation_kind == 1u || operation_kind == 18u || operation_kind == 21u) return push.parameters[slot == 5u ? 31u : (slot >= 8u ? 11u + slot - 8u : (slot == 1u ? 8u : (slot == 3u ? 9u : 10u)))];
    return push.parameters[slot < 8u ? 24u + slot : 14u + slot - 8u];
#endif
}
DEVICE uint rotate_left(uint value, uint shift) { return (value << shift) | (value >> (32u - shift)); }
DEVICE uint swap_bytes(uint value) {
    return (value << 24u) | ((value & 65280u) << 8u) | ((value >> 8u) & 65280u) | (value >> 24u);
}
DEVICE void quarter(REF(uint, first), REF(uint, second), REF(uint, third), REF(uint, fourth)) {
    first += second; fourth = rotate_left(fourth ^ first, 16u);
    third += fourth; second = rotate_left(second ^ third, 12u);
    first += second; fourth = rotate_left(fourth ^ first, 8u);
    third += fourth; second = rotate_left(second ^ third, 7u);
}
DEVICE void chacha(Config config, uint block, OUTPUT(uint, state, 16)) {
    state[0] = 0x61707865u; state[4] = parameter(config, 8u);
    state[8] = parameter(config, 12u); state[12] = block;
    UNROLL for (uint row = 0u; row < 4u; ++row) {
        state[row * 4u + 1u] = parameter(config, 16u + row * 3u);
        state[row * 4u + 2u] = parameter(config, 17u + row * 3u);
        state[row * 4u + 3u] = parameter(config, 18u + row * 3u);
    }
    quarter(state[0], state[4], state[8], state[12]);
    quarter(state[0], state[5], state[10], state[15]); quarter(state[1], state[6], state[11], state[12]);
    quarter(state[2], state[7], state[8], state[13]); quarter(state[3], state[4], state[9], state[14]);
    UNROLL for (uint round = 1u; round < 4u; ++round) {
        quarter(state[0], state[4], state[8], state[12]); quarter(state[1], state[5], state[9], state[13]);
        quarter(state[2], state[6], state[10], state[14]); quarter(state[3], state[7], state[11], state[15]);
        quarter(state[0], state[5], state[10], state[15]); quarter(state[1], state[6], state[11], state[12]);
        quarter(state[2], state[7], state[8], state[13]); quarter(state[3], state[4], state[9], state[14]);
    }
    state[0] += 0x61707865u; state[1] += 0x3320646eu;
    state[2] += 0x79622d32u; state[3] += 0x6b206574u;
    UNROLL for (uint index = 0u; index < 8u; ++index) state[index + 4u] += parameter(config, index + 8u);
    state[12] += block;
    UNROLL for (uint index = 0u; index < 16u; ++index) state[index] = swap_bytes(state[index]);
}
DEVICE void mix(OUTPUT(uint, state, 16), uint first, uint second, uint third, uint fourth, uint low, uint high) {
    state[first] += state[second] + low; state[fourth] = rotate_left(state[fourth] ^ state[first], 16u);
    state[third] += state[fourth]; state[second] = rotate_left(state[second] ^ state[third], 20u);
    state[first] += state[second] + high; state[fourth] = rotate_left(state[fourth] ^ state[first], 24u);
    state[third] += state[fourth]; state[second] = rotate_left(state[second] ^ state[third], 25u);
}
DEVICE void blake(INPUT(uint, message, 16), uint length, OUTPUT(uint, hash, 8)) {
    uint state[16];
    uint iv[8];
    iv[0] = 0x6a09e667u; iv[1] = 0xbb67ae85u; iv[2] = 0x3c6ef372u; iv[3] = 0xa54ff53au;
    iv[4] = 0x510e527fu; iv[5] = 0x9b05688cu; iv[6] = 0x1f83d9abu; iv[7] = 0x5be0cd19u;
    for (uint index = 0u; index < 8u; ++index) state[index] = iv[index];
    for (uint index = 0u; index < 4u; ++index) state[index + 8u] = iv[index];
    state[12] = 0u; state[13] = 0u; state[14] = length; state[15] = 11u;
    mix(state, 0u,4u,8u,12u, message[0], message[1]);
    mix(state, 1u,5u,9u,13u, message[2], message[3]);
    mix(state, 2u,6u,10u,14u, message[4], message[5]);
    mix(state, 3u,7u,11u,15u, message[6], message[7]);
    mix(state, 0u,5u,10u,15u, message[8], message[9]);
    mix(state, 1u,6u,11u,12u, message[10], message[11]);
    mix(state, 2u,7u,8u,13u, message[12], message[13]);
    mix(state, 3u,4u,9u,14u, message[14], message[15]);
    mix(state, 0u,4u,8u,12u, message[2], message[6]);
    mix(state, 1u,5u,9u,13u, message[3], message[10]);
    mix(state, 2u,6u,10u,14u, message[7], message[0]);
    mix(state, 3u,7u,11u,15u, message[4], message[13]);
    mix(state, 0u,5u,10u,15u, message[1], message[11]);
    mix(state, 1u,6u,11u,12u, message[12], message[5]);
    mix(state, 2u,7u,8u,13u, message[9], message[14]);
    mix(state, 3u,4u,9u,14u, message[15], message[8]);
    mix(state, 0u,4u,8u,12u, message[3], message[4]);
    mix(state, 1u,5u,9u,13u, message[10], message[12]);
    mix(state, 2u,6u,10u,14u, message[13], message[2]);
    mix(state, 3u,7u,11u,15u, message[7], message[14]);
    mix(state, 0u,5u,10u,15u, message[6], message[5]);
    mix(state, 1u,6u,11u,12u, message[9], message[0]);
    mix(state, 2u,7u,8u,13u, message[11], message[15]);
    mix(state, 3u,4u,9u,14u, message[8], message[1]);
    mix(state, 0u,4u,8u,12u, message[10], message[7]);
    mix(state, 1u,5u,9u,13u, message[12], message[9]);
    mix(state, 2u,6u,10u,14u, message[14], message[3]);
    mix(state, 3u,7u,11u,15u, message[13], message[15]);
    mix(state, 0u,5u,10u,15u, message[4], message[0]);
    mix(state, 1u,6u,11u,12u, message[11], message[2]);
    mix(state, 2u,7u,8u,13u, message[5], message[8]);
    mix(state, 3u,4u,9u,14u, message[1], message[6]);
    mix(state, 0u,4u,8u,12u, message[12], message[13]);
    mix(state, 1u,5u,9u,13u, message[9], message[11]);
    mix(state, 2u,6u,10u,14u, message[15], message[10]);
    mix(state, 3u,7u,11u,15u, message[14], message[8]);
    mix(state, 0u,5u,10u,15u, message[7], message[2]);
    mix(state, 1u,6u,11u,12u, message[5], message[3]);
    mix(state, 2u,7u,8u,13u, message[0], message[1]);
    mix(state, 3u,4u,9u,14u, message[6], message[4]);
    mix(state, 0u,4u,8u,12u, message[9], message[14]);
    mix(state, 1u,5u,9u,13u, message[11], message[5]);
    mix(state, 2u,6u,10u,14u, message[8], message[12]);
    mix(state, 3u,7u,11u,15u, message[15], message[1]);
    mix(state, 0u,5u,10u,15u, message[13], message[3]);
    mix(state, 1u,6u,11u,12u, message[0], message[10]);
    mix(state, 2u,7u,8u,13u, message[2], message[6]);
    mix(state, 3u,4u,9u,14u, message[4], message[7]);
    mix(state, 0u,4u,8u,12u, message[11], message[15]);
    mix(state, 1u,5u,9u,13u, message[5], message[0]);
    mix(state, 2u,6u,10u,14u, message[1], message[9]);
    mix(state, 3u,7u,11u,15u, message[8], message[6]);
    mix(state, 0u,5u,10u,15u, message[14], message[10]);
    mix(state, 1u,6u,11u,12u, message[2], message[12]);
    mix(state, 2u,7u,8u,13u, message[3], message[4]);
    mix(state, 3u,4u,9u,14u, message[7], message[13]);
    for (uint index = 0u; index < 8u; ++index) hash[index] = swap_bytes(state[index] ^ state[index + 8u]);
}
DEVICE Address value_at(Address nodes, uint index, uint stride) {
    if (stride == 2u) return Address(read_word(nodes, index * 2u)) << 6u | Address(read_word(nodes, index * 2u + 1u) >> 26u);
    return Address(read_word(nodes, index * stride)) | (Address(read_word(nodes, index * stride + 1u)) << 32u);
}
DEVICE Address second_value_at(Config config, Address nodes, uint index) {
    uint capacity = parameter(config, 5u);
    if (capacity == 0u) return value_at(nodes, index, 4u);
    return Address(read_word(nodes, index)) | (Address(index / capacity) << 32u);
}
DEVICE Address partitioned_second_value_at(Config config, Address nodes, uint index, uint job) {
    if (parameter(config, 5u) == 0u) return value_at(nodes, index, 4u);
    uint region = read_word(pointer_at(config, 9u), job * 3u + 2u);
    return Address(read_word(nodes, index)) | (Address(region) << 32u);
}
DEVICE Address second_xs_at(Config config, Address nodes, uint index) {
    uint capacity = parameter(config, 5u);
    if (capacity == 0u) return read_pair(nodes, index * 2u + 1u);
    return read_pair(nodes + Address(capacity) * Address(256u), index);
}
DEVICE void write_partitioned_second(Config config, Address nodes, uint index, uint low, uint high, uint left, uint right) {
    uint capacity = parameter(config, 5u);
    if (capacity == 0u) {
        write_quad(nodes, index, low, high, left, right);
    } else {
        write_word(nodes, index, low);
        write_pair(nodes + Address(capacity) * Address(256u), index, left, right);
    }
}
DEVICE void metadata_at(Address nodes, uint index, uint table, OUTPUT(uint, metadata, 4)) {
    for (uint word = 0u; word < 4u; ++word) metadata[word] = 0u;
    uint stride = table <= 3u ? 4u : 8u;
    uint count = table == 2u ? 1u : (table == 3u ? 2u : 4u);
    for (uint word = 0u; word < count; ++word) metadata[word] = read_word(nodes, index * stride + 2u + word);
}
DEVICE void evaluate_second(Address value, uint left_x, uint right_x, OUTPUT(uint, node, 8)) {
    uint message[16];
    for (uint index = 0u; index < 16u; ++index) message[index] = 0u;
    message[0] = swap_bytes(uint(value >> 6u));
    message[1] = swap_bytes((uint(value) << 26u) | (left_x >> 6u));
    message[2] = swap_bytes((left_x << 26u) | (right_x >> 6u));
    message[3] = (right_x & 63u) << 2u;
    uint hash[8]; blake(message, 13u, hash);
    node[0] = (hash[0] << 6u) | (hash[1] >> 26u); node[1] = hash[0] >> 26u;
    node[2] = left_x; node[3] = right_x; node[4] = 0u; node[5] = 0u;
}
DEVICE void evaluate(Config config, uint left, uint right, uint table, OUTPUT(uint, node, 8)) {
    Address nodes = pointer_at(config, 0u);
    if (table == 2u) {
        Address value = value_at(nodes, left, 4u);
        uint left_x = read_word(nodes, left * 4u + 2u);
        uint right_x = read_word(nodes, right * 4u + 2u);
        evaluate_second(value, left_x, right_x, node);
        node[6] = left; node[7] = right;
        return;
    }
    uint stride = table <= 3u ? 4u : 8u;
    Address value = table == 3u ? second_value_at(config, nodes, left) : value_at(nodes, left, stride);
    uint left_meta[4]; uint right_meta[4];
    if (table == 3u && parameter(config, 5u) != 0u) {
        Address left_xs = second_xs_at(config, nodes, left);
        Address right_xs = second_xs_at(config, nodes, right);
        left_meta[0] = uint(left_xs); left_meta[1] = uint(left_xs >> 32u); left_meta[2] = 0u; left_meta[3] = 0u;
        right_meta[0] = uint(right_xs); right_meta[1] = uint(right_xs >> 32u); right_meta[2] = 0u; right_meta[3] = 0u;
    } else {
        metadata_at(nodes, left, table, left_meta); metadata_at(nodes, right, table, right_meta);
    }
    uint length = table == 2u ? 4u : (table == 3u ? 8u : (table == 6u ? 12u : (table == 7u ? 8u : 16u)));
    uint message[16];
    for (uint index = 0u; index < 16u; ++index) message[index] = 0u;
    for (uint index = 0u; index < 5u; ++index) {
        uint byte_value = uint((value << 2u) >> ((4u - index) * 8u)) & 255u;
        message[index / 4u] |= byte_value << ((index % 4u) * 8u);
    }
    for (uint index = 0u; index < length * 2u; ++index) {
        uint source = index < length ? left_meta[index / 4u] : right_meta[(index - length) / 4u];
        uint byte_value = (source >> ((3u - index % 4u) * 8u)) & 255u;
        uint offset = index + 4u;
        message[offset / 4u] |= (byte_value >> 6u) << ((offset % 4u) * 8u);
        ++offset;
        message[offset / 4u] |= ((byte_value << 2u) & 255u) << ((offset % 4u) * 8u);
    }
    uint hash[8]; blake(message, 5u + length * 2u, hash);
    node[0] = (hash[0] << 6u) | (hash[1] >> 26u); node[1] = hash[0] >> 26u;
    for (uint index = 2u; index < 8u; ++index) node[index] = 0u;
    if (table < 4u) {
        for (uint index = 0u; index < length / 4u; ++index) {
            node[index + 2u] = left_meta[index]; node[index + 2u + length / 4u] = right_meta[index];
        }
    } else {
        uint output_words = table == 4u ? 4u : (table == 5u ? 3u : (table == 6u ? 2u : 0u));
        for (uint index = 0u; index < output_words; ++index) node[index + 2u] = (hash[index + 1u] << 6u) | (hash[index + 2u] >> 26u);
    }
    node[6] = left; node[7] = right;
}
DEVICE void append_node(Config config, INPUT(uint, node, 8), uint stride) {
    Address status = pointer_at(config, 5u);
    uint index = reserve_output(status);
    if (index >= parameter(config, 3u)) { exchange_word(status, 2u, 1u); return; }
    Address output_nodes = pointer_at(config, 1u);
    if (stride == 4u) write_quad(output_nodes, index, node[0], node[1], node[2], node[3]);
    else for (uint word = 0u; word < stride; ++word) write_word(output_nodes, index * stride + word, node[word]);
}
DEVICE uint read_first_bitmap(Address bitmap, uint index) {
#ifdef __CUDACC__
    return __ldg(((const uint*)bitmap) + index);
#else
    return read_word(bitmap, index);
#endif
}
DEVICE bool keep_first(Config config, Address bitmap, uint hash) {
    uint bucket = hash >> 13u;
    uint word_index = bucket / 32u;
    uint word;
    if ((parameter(config, 5u) & 1u) != 0u) {
        uint summary = (read_first_bitmap(bitmap, 16384u + word_index / 4u) >> ((word_index % 4u) * 8u)) & 255u;
        word = summary > 32u ? read_first_bitmap(bitmap, word_index) : (summary == 0u ? 0u : 1u << (summary - 1u));
    } else {
        word = read_first_bitmap(bitmap, word_index);
    }
    uint bit = bucket % 32u;
    bool keep = ((word >> bit) & 1u) != 0u;
    if (!keep && bucket > 0u && (hash & 8191u) < 512u) {
        uint previous = bit == 0u ? read_first_bitmap(bitmap, bucket / 32u - 1u) : word;
        keep = ((previous >> ((bit + 31u) & 31u)) & 1u) != 0u;
    }
    return keep;
}
DEVICE void emit_first(Config config, uint index, uint hash, uint proof_x) {
    write_word(pointer_at(config, 1u), index * 2u, hash);
    write_word(pointer_at(config, 1u), index * 2u + 1u, proof_x);
    Address histogram = pointer_at(config, 3u);
    if (histogram != Address(0u)) {
        uint bucket = uint((Address(hash) << 6u | Address(proof_x >> 26u)) / Address(15113u));
        if (bucket >= 18188177u) exchange_word(pointer_at(config, 5u), 2u, 3u);
        else if (add_word(histogram, bucket, 1u) >= 1024u) exchange_word(pointer_at(config, 5u), 2u, 2u);
    }
}
DEVICE uint first_bucket(uint hash, uint proof_x) {
    uint quotient = hash / 15113u;
    uint remainder = hash - quotient * 15113u;
    return quotient * 64u + (remainder * 64u + (proof_x >> 26u)) / 15113u;
}
#ifdef __CUDACC__
__forceinline__
#endif
DEVICE void generate(Config config, uint invocation, uint lane) {
#ifdef __CUDACC__
    __shared__ uint generation_words[512], match_count, output_base;
#endif
    uint block = parameter(config, 4u) + invocation;
    uint stream[16]; chacha(config, block, stream);
    Address bitmap = pointer_at(config, 6u);
    UNROLL for (uint segment = 0u; segment < 16u; segment += 8u) {
    if (lane == 0u) match_count = 0u;
    BARRIER();
    uint keep_mask = 0u;
    UNROLL for (uint offset = 0u; offset < 8u; ++offset) {
        if (keep_first(config, bitmap, stream[segment + offset]) && invocation < parameter(config, 1u)) keep_mask |= 1u << offset;
    }
#ifdef __CUDACC__
    uint kept_count = uint(__popc(keep_mask));
    uint subgroup_lane = lane & 31u;
    uint inclusive = kept_count;
    UNROLL for (uint distance = 1u; distance < 32u; distance *= 2u) {
        uint previous = __shfl_up_sync(0xffffffffu, inclusive, distance);
        if (subgroup_lane >= distance) inclusive += previous;
    }
    uint subgroup_count = __shfl_sync(0xffffffffu, inclusive, 31u);
    uint subgroup_base = 0u;
    if (subgroup_lane == 0u) subgroup_base = atomicAdd(&match_count, subgroup_count);
    uint first_slot = __shfl_sync(0xffffffffu, subgroup_base, 0u) + inclusive - kept_count;
#else
    uint kept_count = uint(bitCount(keep_mask));
    uint subgroup_count = subgroupAdd(kept_count);
    uint prefix = subgroupExclusiveAdd(kept_count);
    uint subgroup_base = 0u;
    if (subgroupElect()) subgroup_base = atomicAdd(match_count, subgroup_count);
    uint first_slot = subgroupBroadcastFirst(subgroup_base) + prefix;
#endif
    uint slot = first_slot;
    UNROLL for (uint offset = 0u; offset < 8u; ++offset) {
        if ((keep_mask & (1u << offset)) != 0u) {
            if (slot < 256u) {
                generation_words[slot * 2u] = stream[segment + offset];
                generation_words[slot * 2u + 1u] = block * 16u + segment + offset;
            }
            ++slot;
        }
    }
    BARRIER();
    if (lane == 0u) {
        output_base = add_word(pointer_at(config, 5u), 0u, match_count);
    }
    BARRIER();
    if (match_count > 256u) {
        slot = first_slot;
        UNROLL for (uint offset = 0u; offset < 8u; ++offset) {
            if ((keep_mask & (1u << offset)) != 0u) {
                uint output_index = output_base + slot++;
                if (output_index >= parameter(config, 3u)) exchange_word(pointer_at(config, 5u), 2u, 1u);
                else {
                    emit_first(config, output_index, stream[segment + offset], block * 16u + segment + offset);
                }
            }
        }
    } else {
    for (uint index = lane; index < match_count; index += group_width()) {
        uint output_index = output_base + index;
        if (output_index >= parameter(config, 3u)) {
            exchange_word(pointer_at(config, 5u), 2u, 1u);
            continue;
        }
        uint hash = generation_words[index * 2u];
        uint proof_x = generation_words[index * 2u + 1u];
        emit_first(config, output_index, hash, proof_x);
    }
    }
    BARRIER();
    }
}
#ifdef __CUDACC__
template <bool wide>
DEVICE __forceinline__ void generate_partitioned(Config config, uint invocation, uint lane) {
    __shared__ uint first_partition_records[1024], first_partition_counts[wide ? 64 : 32], first_partition_bases[wide ? 64 : 32];
#else
DEVICE void generate_partitioned(Config config, uint invocation, uint lane, bool wide) {
#endif
    uint region_shift = wide ? 26u : 27u;
    uint slot_bits = wide ? 3u : 4u;
    uint slots = 1u << slot_bits;
    uint regions = wide ? 64u : 32u;
    uint block = parameter(config, 4u) + invocation;
    uint stream[16]; chacha(config, block, stream);
    Address bitmap = pointer_at(config, 6u);
    Address ranges = pointer_at(config, 3u);
    Address output_nodes = pointer_at(config, 1u);
    UNROLL for (uint segment = 0u; segment < 16u; segment += 8u) {
        if (lane < regions) first_partition_counts[lane] = 0u;
        BARRIER();
        uint keep_mask = 0u;
        uint local_slots[8];
        UNROLL for (uint offset = 0u; offset < 8u; ++offset) {
            uint hash = stream[segment + offset];
            local_slots[offset] = 0u;
            if (invocation < parameter(config, 1u) && keep_first(config, bitmap, hash)) {
                keep_mask |= 1u << offset;
                uint region = hash >> region_shift;
#ifdef __CUDACC__
                uint local_index = atomicAdd(&first_partition_counts[region], 1u);
#else
                uint local_index = atomicAdd(first_partition_counts[region], 1u);
#endif
                local_slots[offset] = local_index;
                if (local_index < slots) {
                    uint destination = ((region << slot_bits) + local_index) * 2u;
                    first_partition_records[destination] = hash;
                    first_partition_records[destination + 1u] = block * 16u + segment + offset;
                }
            }
        }
        BARRIER();
        if (lane < regions) {
            uint count = first_partition_counts[lane];
            uint base = count == 0u ? 0u : add_word(ranges, lane * 3u + 2u, count);
            first_partition_bases[lane] = base;
            uint capacity = read_word(ranges, lane * 3u + 1u);
            if (base > capacity || count > capacity - base) exchange_word(pointer_at(config, 5u), 2u, 4u);
        }
        BARRIER();
        for (uint work = lane; work < 512u; work += group_width()) {
            uint region = work >> slot_bits;
            uint local_index = work & (slots - 1u);
            uint count = first_partition_counts[region];
            uint offset = first_partition_bases[region] + local_index;
            if (count <= slots && local_index < count && offset < read_word(ranges, region * 3u + 1u)) {
                uint destination = read_word(ranges, region * 3u) + offset;
                write_pair(output_nodes, destination, first_partition_records[work * 2u], first_partition_records[work * 2u + 1u]);
            }
        }
        UNROLL for (uint offset = 0u; offset < 8u; ++offset) {
            uint hash = stream[segment + offset];
            uint region = hash >> region_shift;
            if ((keep_mask & (1u << offset)) != 0u && first_partition_counts[region] > slots) {
                uint index = first_partition_bases[region] + local_slots[offset];
                if (index < read_word(ranges, region * 3u + 1u)) {
                    uint destination = read_word(ranges, region * 3u) + index;
                    write_pair(output_nodes, destination, hash, block * 16u + segment + offset);
                }
            }
        }
        BARRIER();
    }
}
DEVICE void index_nodes(Config config, uint invocation) {
    if (invocation >= parameter(config, 1u)) return;
    uint stride = parameter(config, 2u) <= 2u ? 4u : 8u;
    Address value = value_at(pointer_at(config, 0u), invocation, stride);
    uint bucket = uint(value / Address(15113u));
    if (bucket >= 18188177u) { exchange_word(pointer_at(config, 5u), 2u, 3u); return; }
    uint previous = exchange_word(pointer_at(config, 2u), bucket, invocation);
    uint link_stride = parameter(config, 2u) == 1u ? 1u : 2u;
    write_word(pointer_at(config, 3u), invocation * link_stride, previous);
    if (link_stride == 2u) write_word(pointer_at(config, 3u), invocation * 2u + 1u, uint(value - Address(bucket) * Address(15113u)));
    if (previous == 0xffffffffu && parameter(config, 2u) == 1u) {
        uint active_index = add_word(pointer_at(config, 5u), 1u, 1u);
        write_word(pointer_at(config, 4u), active_index, bucket);
    }
}
DEVICE bool unique_fifth(Config config, uint entry, uint bucket) {
    if (parameter(config, 2u) != 6u) return true;
    Address nodes = pointer_at(config, 0u);
    Address leaves = pointer_at(config, 10u);
    Address value = value_at(nodes, entry, 8u);
    uint other = read_word(pointer_at(config, 2u), bucket);
    uint visited = 0u;
    while (other != 0xffffffffu && visited++ < 1024u) {
        if (other < entry && value_at(nodes, other, 8u) == value) {
            bool identical = true;
            for (uint index = 6u; index < 22u; ++index) {
                if (read_word(leaves, other * 22u + index) != read_word(leaves, entry * 22u + index)) identical = false;
            }
            if (identical) return false;
        }
        other = read_word(pointer_at(config, 3u), other * 2u);
    }
    if (other != 0xffffffffu) exchange_word(pointer_at(config, 5u), 2u, 2u);
    return true;
}
DEVICE void match_large_bucket(Config config, uint bucket, uint lane, uint left_count, uint right_count) {
    Address packed = pointer_at(config, 6u);
    uint left_start = read_word(pointer_at(config, 2u), bucket);
    uint right_start = read_word(pointer_at(config, 2u), bucket + 1u);
    for (uint index = lane; index < left_count; index += group_width()) {
        Address left_row = read_pair(packed, left_start + index);
        uint left_x = uint(left_row >> 32u);
        Address value = Address(uint(left_row)) << 6u | Address(left_x >> 26u);
        uint remainder = uint(value - Address(bucket) * Address(15113u));
        for (uint right_index = 0u; right_index < right_count; ++right_index) {
            Address right_row = read_pair(packed, right_start + right_index);
            uint right_x = uint(right_row >> 32u);
            Address right_value = Address(uint(right_row)) << 6u | Address(right_x >> 26u);
            uint right_remainder = uint(right_value - Address(bucket + 1u) * Address(15113u));
            uint target_index = (right_remainder / 127u + 119u - remainder / 127u) % 119u;
            uint parity_term = 2u * target_index + (bucket & 1u);
            if (target_index < 64u && (parity_term * parity_term + remainder) % 127u == right_remainder % 127u) {
                uint node[8]; node[0] = uint(value); node[1] = uint(value >> 32u);
                node[2] = left_x; node[3] = right_x;
                append_node(config, node, 4u);
            }
        }
    }
}
DEVICE void match_nodes(Config config, uint group, uint lane) {
#ifdef __CUDACC__
#ifdef GH_CUDA_PACKED_DENSE
    __shared__ uint left_entries[304], left_values[304], right_entries[304];
    __shared__ uint dense_slots[2048], dense_collisions[32], collision_count;
#else
    __shared__ uint left_entries[304], left_values[304], right_entries[304], right_values[304], hash_heads[512];
    __shared__ uint right_present[473];
#endif
    __shared__ uint matches[512], match_count, output_base;
    __shared__ uint left_count, right_count;
#endif
    uint active_index = group + parameter(config, 4u);
    if (active_index >= parameter(config, 1u)) return;
    uint bucket = read_word(pointer_at(config, 4u), active_index);
    if (bucket >= 18188176u) return;
    Address heads = pointer_at(config, 2u); Address counts = pointer_at(config, 3u);
    Address packed = pointer_at(config, 6u);
    if (lane == 0u) {
        match_count = 0u;
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
        collision_count = 0u;
#endif
        left_count = read_word(counts, bucket) & 65535u;
        right_count = read_word(counts, bucket + 1u) & 65535u;
    }
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
    for (uint index = lane; index < 2048u; index += group_width()) dense_slots[index] = 0xffffffffu;
#else
    for (uint index = lane; index < 512u; index += group_width()) hash_heads[index] = 0xffffffffu;
    for (uint index = lane; index < 473u; index += group_width()) right_present[index] = 0u;
#endif
    BARRIER();
    if (left_count == 0u || right_count == 0u) return;
    if (left_count > 304u || right_count > 304u) {
        match_large_bucket(config, bucket, lane, left_count, right_count);
        return;
    }
    Address right_base = Address(bucket + 1u) * Address(15113u);
    uint left_start = read_word(heads, bucket);
    uint right_start = read_word(heads, bucket + 1u);
    for (uint index = lane; index < left_count; index += group_width()) {
        Address row = read_pair(packed, left_start + index);
        uint hash = uint(row);
        uint proof_x = uint(row >> 32u);
        left_entries[index] = proof_x;
        uint remainder = uint((Address(hash) << 6u | Address(proof_x >> 26u)) - Address(bucket) * Address(15113u));
        left_values[index] = (remainder / 127u) * 128u + remainder % 127u;
    }
    for (uint index = lane; index < right_count; index += group_width()) {
        Address row = read_pair(packed, right_start + index);
        uint hash = uint(row);
        uint proof_x = uint(row >> 32u);
        right_entries[index] = proof_x;
        uint remainder = uint((Address(hash) << 6u | Address(proof_x >> 26u)) - right_base);
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
        uint packed_index = (remainder & 4095u) >> 1u;
        uint shift = (remainder & 1u) * 16u;
        uint entry = (remainder & 65024u) | index;
        uint expected = 0xffffffffu;
        while (true) {
            if (((expected >> shift) & 65535u) != 65535u) {
                uint collision_index = atomicInc(&collision_count, 0x7fffffffu);
                if (collision_index < 32u) dense_collisions[collision_index] = remainder | (index << 16u);
                break;
            }
            uint desired = (expected & ~(65535u << shift)) | (entry << shift);
            uint previous = atomicCAS(&dense_slots[packed_index], expected, desired);
            if (previous == expected) break;
            expected = previous;
        }
#else
#ifdef __CUDACC__
        uint previous = atomicExch(&hash_heads[remainder & 511u], index);
        atomicOr(&right_present[remainder >> 5u], 1u << (remainder & 31u));
#else
        uint previous = atomicExchange(hash_heads[remainder & 511u], index);
        atomicOr(right_present[remainder >> 5u], 1u << (remainder & 31u));
#endif
        right_values[index] = remainder | ((previous & 65535u) << 16u);
#endif
    }
    BARRIER();
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
    if (collision_count > 32u) {
        match_large_bucket(config, bucket, lane, left_count, right_count);
        return;
    }
#endif
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE) && GH_CUDA_DENSE_TARGETS == 8
    uint target_start = (lane & 7u) * 8u;
    uint packed_delta_low = 0u;
    uint packed_delta_high = 0u;
    UNROLL
    for (uint target_offset = 0u; target_offset < 8u; ++target_offset) {
        uint parity_term = 2u * (target_start + target_offset) + (bucket & 1u);
        uint square = parity_term * parity_term;
        uint delta = (square & 127u) + (square >> 7u);
        if (delta >= 127u) delta -= 127u;
        if (target_offset < 4u) packed_delta_low |= delta << (target_offset * 8u);
        else packed_delta_high |= delta << ((target_offset - 4u) * 8u);
    }
    for (uint work = lane; work < left_count * 8u; work += group_width()) {
        uint index = work >> 3u;
#else
    uint target_index = lane & 63u;
    uint parity_term = 2u * target_index + (bucket & 1u);
    uint target_column_delta = (parity_term * parity_term) % 127u;
    for (uint work = lane; work < left_count * 64u; work += group_width()) {
        uint index = work >> 6u;
#endif
        uint row = left_values[index] >> 7u;
        uint column = left_values[index] & 127u;
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE) && GH_CUDA_DENSE_TARGETS == 8
        UNROLL
        for (uint target_offset = 0u; target_offset < 8u; ++target_offset) {
            uint target_index = target_start + target_offset;
            uint packed_delta = target_offset < 4u ? packed_delta_low : packed_delta_high;
            uint target_column_delta = (packed_delta >> ((target_offset & 3u) * 8u)) & 255u;
#endif
        uint target_row = row + target_index;
        if (target_row >= 119u) target_row -= 119u;
        uint target_column = column + target_column_delta;
        if (target_column >= 127u) target_column -= 127u;
        uint target = target_row * 127u + target_column;
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
        uint entry = ((const unsigned short*)dense_slots)[target & 4095u];
        uint match_index = entry & 511u;
        if (match_index < right_count && ((entry ^ target) & 65024u) == 0u) {
            uint slot = atomicInc(&match_count, 0x7fffffffu);
            if (slot < 512u) matches[slot] = index | (match_index << 10u);
        }
#else
        if ((right_present[target >> 5u] & (1u << (target & 31u))) == 0u) continue;
        uint match_index = hash_heads[target & 511u];
        while (match_index < 304u) {
            if ((right_values[match_index] & 65535u) == target) {
#ifdef __CUDACC__
                uint slot = atomicInc(&match_count, 0x7fffffffu);
#else
                uint slot = atomicAdd(match_count, 1u);
#endif
                if (slot < 512u) matches[slot] = index | (match_index << 10u);
            }
            match_index = right_values[match_index] >> 16u;
        }
#endif
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE) && GH_CUDA_DENSE_TARGETS == 8
        }
#endif
    }
#if defined(__CUDACC__) && defined(GH_CUDA_PACKED_DENSE)
    for (uint work = lane; work < left_count * collision_count; work += group_width()) {
        uint index = work / collision_count;
        uint collision = dense_collisions[work - index * collision_count];
        uint right_remainder = collision & 65535u;
        uint row = left_values[index] >> 7u;
        uint column = left_values[index] & 127u;
        uint target_index = right_remainder / 127u + 119u - row;
        if (target_index >= 119u) target_index -= 119u;
        if (target_index < 64u) {
            uint parity_term = 2u * target_index + (bucket & 1u);
            uint square_column = parity_term * parity_term + column;
            uint target_column = (square_column & 127u) + (square_column >> 7u);
            if (target_column >= 127u) target_column -= 127u;
            if (target_column == right_remainder % 127u) {
                uint slot = atomicInc(&match_count, 0x7fffffffu);
                if (slot < 512u) matches[slot] = index | ((collision >> 16u) << 10u);
            }
        }
    }
#endif
    BARRIER();
    if (match_count > 512u) {
        match_large_bucket(config, bucket, lane, left_count, right_count);
        return;
    }
    if (lane == 0u) output_base = add_word(pointer_at(config, 5u), 0u, match_count);
    BARRIER();
    for (uint index = lane; index < match_count; index += group_width()) {
        uint output_index = output_base + index;
        if (output_index >= parameter(config, 3u)) {
            exchange_word(pointer_at(config, 5u), 2u, 1u);
            continue;
        }
        uint pair = matches[index];
        uint left_index = pair & 1023u;
        uint remainder = (left_values[left_index] >> 7u) * 127u + (left_values[left_index] & 127u);
        Address value = Address(bucket) * Address(15113u) + Address(remainder);
        Address output_nodes = pointer_at(config, 1u);
        write_quad(output_nodes, output_index, uint(value), uint(value >> 32u), left_entries[left_index], right_entries[pair >> 10u]);
    }
}
DEVICE void gather_fifth(Config config, uint invocation) {
    if (invocation >= parameter(config, 1u)) return;
    Address fifth = pointer_at(config, 0u); Address output_nodes = pointer_at(config, 1u);
    Address second = pointer_at(config, 7u); Address third = pointer_at(config, 8u); Address fourth = pointer_at(config, 9u);
    for (uint index = 0u; index < 6u; ++index) write_word(output_nodes, invocation * 22u + index, read_word(fifth, invocation * 8u + index));
    for (uint branch = 0u; branch < 8u; ++branch) {
        uint fourth_index = read_word(fifth, invocation * 8u + 6u + branch / 4u);
        uint third_index = read_word(fourth, fourth_index * 8u + 6u + (branch / 2u) % 2u);
        uint second_index = read_word(third, third_index * 8u + 6u + branch % 2u);
        Address proof_xs = second_xs_at(config, second, second_index);
        write_word(output_nodes, invocation * 22u + 6u + branch * 2u, uint(proof_xs));
        write_word(output_nodes, invocation * 22u + 7u + branch * 2u, uint(proof_xs >> 32u));
    }
}
#ifdef __CUDACC__
__forceinline__
#endif
DEVICE void match_sparse(Config config, uint group, uint lane, uint table) {
    uint left;
    uint bucket;
    uint remainder;
    uint stride = table == 3u ? 4u : 8u;
    Address nodes = pointer_at(config, 0u);
    if (table == 3u) {
        uint job = group + parameter(config, 4u);
        if (job >= parameter(config, 6u)) return;
        Address jobs = pointer_at(config, 11u);
        if (lane >= read_word(jobs, job * 3u + 1u)) return;
        uint index = read_word(jobs, job * 3u) + lane;
        uint relative = read_word(pointer_at(config, 6u), index * 2u);
        bucket = read_word(jobs, job * 3u + 2u) + relative / 15113u;
        remainder = relative % 15113u;
        left = read_word(pointer_at(config, 6u), index * 2u + 1u);
    } else {
        left = group * 128u + lane + parameter(config, 4u);
        if (left >= parameter(config, 1u)) return;
        Address left_value = value_at(nodes, left, stride);
        bucket = uint(left_value / Address(15113u));
        remainder = uint(left_value - Address(bucket) * Address(15113u));
    }
    if (bucket >= 18188176u || (table != 3u && !unique_fifth(config, left, bucket))) return;
    uint right = read_word(pointer_at(config, 2u), bucket + 1u);
    uint packed_start = right;
    uint packed_count = table == 3u ? read_word(pointer_at(config, 3u), bucket + 1u) & 65535u : 0u;
    uint visited = 0u;
    while ((table == 3u ? visited < packed_count : right != 0xffffffffu) && visited < 1024u) {
        uint right_remainder;
        if (table == 3u) {
            right_remainder = read_word(pointer_at(config, 6u), (packed_start + visited) * 2u) % 15113u;
            right = read_word(pointer_at(config, 6u), (packed_start + visited) * 2u + 1u);
        } else {
            right_remainder = read_word(pointer_at(config, 3u), right * 2u + 1u);
        }
        ++visited;
        uint target_index = (right_remainder / 127u + 119u - remainder / 127u) % 119u;
        uint parity_term = 2u * target_index + (bucket & 1u);
        if (target_index < 64u && (parity_term * parity_term + remainder) % 127u == right_remainder % 127u
            && (table == 3u || unique_fifth(config, right, bucket + 1u))) {
            uint node[8];
            if (table == 3u) {
                for (uint word = 0u; word < 6u; ++word) node[word] = 0u;
                node[6] = left; node[7] = right;
            } else {
                evaluate(config, left, right, table, node);
            }
            append_node(config, node, 8u);
        }
        if (table != 3u) right = read_word(pointer_at(config, 3u), right * 2u);
    }
    if (table == 3u ? visited < packed_count : right != 0xffffffffu) exchange_word(pointer_at(config, 5u), 2u, 2u);
}
DEVICE void gather_proofs(Config config, uint invocation) {
    if (invocation >= parameter(config, 1u)) return;
    Address seventh = pointer_at(config, 0u); Address output_nodes = pointer_at(config, 1u);
    if (uint(value_at(seventh, invocation, 8u) >> 6u) != parameter(config, 5u)) return;
    Address status = pointer_at(config, 5u);
    uint output_index = add_word(status, 0u, 1u);
    if (output_index >= parameter(config, 3u)) { exchange_word(status, 2u, 1u); return; }
    Address sixth = pointer_at(config, 7u); Address leaves = pointer_at(config, 10u);
    for (uint branch = 0u; branch < 4u; ++branch) {
        uint sixth_index = read_word(seventh, invocation * 8u + 6u + branch / 2u);
        uint fifth_index = read_word(sixth, sixth_index * 8u + 6u + branch % 2u);
        for (uint index = 0u; index < 16u; ++index) write_word(output_nodes, output_index * 64u + branch * 16u + index, read_word(leaves, fifth_index * 22u + 6u + index));
    }
}
DEVICE void bucket_offsets(Config config, uint invocation, uint lane) {
#ifdef __CUDACC__
    __shared__ uint prefix[128], base, job_base;
#else
#define prefix left_entries
#define base left_count
#define job_base right_count
#endif
    uint count = invocation < parameter(config, 1u) ? read_word(pointer_at(config, 3u), invocation) : 0u;
    prefix[lane] = count;
    BARRIER();
    for (uint distance = 1u; distance < 128u; distance *= 2u) {
        uint previous = lane >= distance ? prefix[lane - distance] : 0u;
        BARRIER();
        prefix[lane] += previous;
        BARRIER();
    }
    if (lane == 0u) {
        base = add_word(pointer_at(config, 5u), 3u, prefix[127]);
        job_base = add_word(pointer_at(config, 5u), 1u, (prefix[127] + 127u) / 128u);
    }
    BARRIER();
    if (invocation < parameter(config, 1u)) write_word(pointer_at(config, 2u), invocation, base + prefix[lane]);
    for (uint chunk = lane; chunk < (prefix[127] + 127u) / 128u; chunk += 128u) {
        uint remaining = prefix[127] - chunk * 128u;
        write_word(pointer_at(config, 11u), (job_base + chunk) * 3u, base + chunk * 128u);
        write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
        write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 2u, invocation - lane);
    }
#ifndef __CUDACC__
#undef prefix
#undef base
#undef job_base
#endif
}
DEVICE void coarse_offsets(Config config, uint group, uint lane) {
    if (parameter(config, 6u) == 5u) {
        if (group >= parameter(config, 1u)) return;
        uint job_base = 0u;
        for (uint partition_index = 0u; partition_index < group; ++partition_index) job_base += (read_word(pointer_at(config, 7u), partition_index * 3u + 2u) + 127u) / 128u;
        uint start = read_word(pointer_at(config, 7u), group * 3u);
        uint count = read_word(pointer_at(config, 7u), group * 3u + 2u);
        for (uint chunk = lane; chunk < (count + 127u) / 128u; chunk += 128u) {
            uint remaining = count - chunk * 128u;
            write_word(pointer_at(config, 9u), (job_base + chunk) * 3u, start + chunk * 128u);
            write_word(pointer_at(config, 9u), (job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
            write_word(pointer_at(config, 9u), (job_base + chunk) * 3u + 2u, group);
        }
        return;
    }
    uint size = 1u << parameter(config, 7u);
    if (parameter(config, 6u) == 3u) {
        uint coarse = group * 128u + lane;
        if (coarse >= (parameter(config, 1u) + size - 1u) / size) return;
        uint count = read_word(pointer_at(config, 9u), coarse);
        uint start = add_word(pointer_at(config, 5u), 3u, count);
        write_word(pointer_at(config, 7u), coarse * 2u, start + count);
        write_word(pointer_at(config, 7u), coarse * 2u + 1u, count);
        return;
    }
    if (group >= (parameter(config, 1u) + size - 1u) / size) return;
#ifdef __CUDACC__
    __shared__ uint sums[128], job_base, data_base;
#else
#define sums left_entries
#define job_base left_count
#define data_base right_count
#endif
    uint count = 0u;
    for (uint index = group * size + lane; index < (group + 1u) * size && index < parameter(config, 1u); index += 128u) {
        count += read_word(pointer_at(config, 3u), index);
    }
    sums[lane] = count;
    BARRIER();
    for (uint distance = 64u; distance > 0u; distance /= 2u) {
        if (lane < distance) sums[lane] += sums[lane + distance];
        BARRIER();
    }
    if (lane == 0u) {
        uint offset = add_word(pointer_at(config, 5u), 3u, sums[0]);
        data_base = offset;
        job_base = add_word(pointer_at(config, 5u), 1u, (sums[0] + 127u) / 128u);
        write_word(pointer_at(config, 7u), group * 2u, offset + sums[0]);
        write_word(pointer_at(config, 7u), group * 2u + 1u, sums[0]);
    }
    BARRIER();
    for (uint chunk = lane; chunk < (sums[0] + 127u) / 128u; chunk += 128u) {
        uint remaining = sums[0] - chunk * 128u;
        write_word(pointer_at(config, 9u), (job_base + chunk) * 3u, data_base + chunk * 128u);
        write_word(pointer_at(config, 9u), (job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
        write_word(pointer_at(config, 9u), (job_base + chunk) * 3u + 2u, group);
    }
#ifndef __CUDACC__
#undef sums
#undef job_base
#undef data_base
#endif
}
DEVICE void local_compact(Config config, uint group, uint lane) {
    uint size = 1u << parameter(config, 7u);
    if (group >= (18188177u + size - 1u) / size) return;
    uint start = read_word(pointer_at(config, 7u), group * 2u);
    uint count = read_word(pointer_at(config, 7u), group * 2u + 1u);
    if (size > 1024u || count > size * 1024u) {
        if (lane == 0u) exchange_word(pointer_at(config, 5u), 2u, 2u);
        return;
    }
#ifdef __CUDACC__
    __shared__ uint local_counts[1024], prefix[128], scatter_job_base;
#else
#define prefix left_entries
#define scatter_job_base output_base
#endif
    uint scatter_job_count = (count + 127u) / 128u;
    if (lane == 0u) scatter_job_base = add_word(pointer_at(config, 5u), 0u, scatter_job_count);
    for (uint bucket = lane; bucket < size; bucket += 128u) local_counts[bucket] = 0u;
    BARRIER();
    for (uint chunk = lane; chunk < scatter_job_count; chunk += 128u) {
        uint remaining = count - chunk * 128u;
        write_word(pointer_at(config, 9u), (scatter_job_base + chunk) * 3u, start + chunk * 128u);
        write_word(pointer_at(config, 9u), (scatter_job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
        write_word(pointer_at(config, 9u), (scatter_job_base + chunk) * 3u + 2u, group);
    }
    for (uint index = lane; index < count; index += 128u) {
        uint relative = read_word(pointer_at(config, 8u), (start + index) * 2u);
        uint bucket = relative / 15113u;
        if (bucket >= size || group * size + bucket >= 18188177u) {
            exchange_word(pointer_at(config, 5u), 2u, 3u);
            continue;
        }
#ifdef __CUDACC__
        uint previous = atomicAdd(&local_counts[bucket], 1u);
#else
        uint previous = atomicAdd(local_counts[bucket], 1u);
#endif
        if (previous >= 1024u) exchange_word(pointer_at(config, 5u), 2u, 2u);
    }
    BARRIER();
    uint width = size / 128u;
    uint total = 0u;
    for (uint index = 0u; index < width; ++index) total += local_counts[lane * width + index];
    prefix[lane] = total;
    BARRIER();
    for (uint distance = 1u; distance < 128u; distance *= 2u) {
        uint previous = lane >= distance ? prefix[lane - distance] : 0u;
        BARRIER();
        prefix[lane] += previous;
        BARRIER();
    }
    uint offset = start + prefix[lane] - total;
    for (uint index = 0u; index < width; ++index) {
        uint bucket = lane * width + index;
        uint entries = local_counts[bucket];
        if (group * size + bucket < 18188177u) {
            write_word(pointer_at(config, 3u), group * size + bucket, entries);
            write_word(pointer_at(config, 2u), group * size + bucket, offset + entries);
        }
        local_counts[bucket] = offset;
        offset += entries;
    }
    BARRIER();
    for (uint partition_index = lane; partition_index < size / 128u; partition_index += 128u) {
        uint partition_start = local_counts[partition_index * 128u];
        uint partition_end = (partition_index + 1u) * 128u < size
            ? local_counts[(partition_index + 1u) * 128u] : start + count;
        uint partition_count = partition_end - partition_start;
        uint job_count = (partition_count + 127u) / 128u;
        uint job_base = add_word(pointer_at(config, 5u), 1u, job_count);
        for (uint chunk = 0u; chunk < job_count; ++chunk) {
            uint remaining = partition_count - chunk * 128u;
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u, partition_start + chunk * 128u);
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 2u, group * size + partition_index * 128u);
        }
    }
#ifndef __CUDACC__
#undef prefix
#undef scatter_job_base
#endif
}
DEVICE void local_compact_coalesced(Config config, uint group, uint lane) {
    if (group >= (18188177u + 511u) / 512u) return;
    uint start = read_word(pointer_at(config, 7u), group * 2u);
    uint count = read_word(pointer_at(config, 7u), group * 2u + 1u);
    if (parameter(config, 7u) != 9u || count > 512u * 1024u) {
        if (lane == 0u) exchange_word(pointer_at(config, 5u), 2u, 2u);
        return;
    }
#ifdef __CUDACC__
    __shared__ uint tile_counts[512], tile_prefix[128], tile_records[8192], tile_error, tile_job_base;
#endif
    for (uint bucket = lane; bucket < 512u; bucket += group_width()) tile_counts[bucket] = 0u;
    if (lane == 0u) tile_error = 0u;
    BARRIER();
#ifdef __CUDACC__
#define LOCAL_TILE_SLOTS 16u
#else
#define LOCAL_TILE_SLOTS (4096u / gl_WorkGroupSize.x)
#endif
    uint values[LOCAL_TILE_SLOTS];
    uint sources[LOCAL_TILE_SLOTS];
    bool retained = count <= 4096u;
    if (retained) {
        UNROLL for (uint slot = 0u; slot < LOCAL_TILE_SLOTS; ++slot) {
            uint index = lane + slot * group_width();
            if (index < count) {
                Address pair = read_pair(pointer_at(config, 8u), start + index);
                uint relative = uint(pair);
                values[slot] = relative;
                sources[slot] = uint(pair >> 32u);
                uint bucket = relative / 15113u;
                if (bucket >= 512u || group * 512u + bucket >= 18188177u) {
                    exchange_word(pointer_at(config, 5u), 2u, 3u);
#ifdef __CUDACC__
                    atomicExch(&tile_error, 1u);
#else
                    atomicExchange(tile_error, 1u);
#endif
                } else {
#ifdef __CUDACC__
                    uint previous = atomicAdd(&tile_counts[bucket], 1u);
#else
                    uint previous = atomicAdd(tile_counts[bucket], 1u);
#endif
                    if (previous >= 1024u) {
                        exchange_word(pointer_at(config, 5u), 2u, 2u);
#ifdef __CUDACC__
                        atomicExch(&tile_error, 1u);
#else
                        atomicExchange(tile_error, 1u);
#endif
                    }
                }
            }
        }
    } else {
        for (uint index = lane; index < count; index += group_width()) {
            uint relative = read_word(pointer_at(config, 8u), (start + index) * 2u);
            uint bucket = relative / 15113u;
            if (bucket >= 512u || group * 512u + bucket >= 18188177u) {
                exchange_word(pointer_at(config, 5u), 2u, 3u);
#ifdef __CUDACC__
                atomicExch(&tile_error, 1u);
#else
                atomicExchange(tile_error, 1u);
#endif
            } else {
#ifdef __CUDACC__
                uint previous = atomicAdd(&tile_counts[bucket], 1u);
#else
                uint previous = atomicAdd(tile_counts[bucket], 1u);
#endif
                if (previous >= 1024u) {
                    exchange_word(pointer_at(config, 5u), 2u, 2u);
#ifdef __CUDACC__
                    atomicExch(&tile_error, 1u);
#else
                    atomicExchange(tile_error, 1u);
#endif
                }
            }
        }
    }
    BARRIER();
    if (tile_error != 0u) return;
    uint total = 0u;
    if (lane < 128u) {
        for (uint index = 0u; index < 4u; ++index) total += tile_counts[lane * 4u + index];
        tile_prefix[lane] = total;
    }
    BARRIER();
    for (uint distance = 1u; distance < 128u; distance *= 2u) {
        uint previous = lane < 128u && lane >= distance ? tile_prefix[lane - distance] : 0u;
        BARRIER();
        if (lane < 128u) tile_prefix[lane] += previous;
        BARRIER();
    }
    if (lane < 128u) {
        uint offset = tile_prefix[lane] - total;
        for (uint index = 0u; index < 4u; ++index) {
            uint bucket = lane * 4u + index;
            uint entries = tile_counts[bucket];
            if (group * 512u + bucket < 18188177u) {
                write_word(pointer_at(config, 3u), group * 512u + bucket, entries);
                write_word(pointer_at(config, 2u), group * 512u + bucket, start + offset + (retained ? 0u : entries));
            }
            tile_counts[bucket] = offset;
            offset += entries;
        }
    }
    BARRIER();
    if (lane < 4u) {
        uint partition_start = start + tile_counts[lane * 128u];
        uint partition_end = lane < 3u ? start + tile_counts[(lane + 1u) * 128u] : start + count;
        uint partition_count = partition_end - partition_start;
        uint job_count = (partition_count + 127u) / 128u;
        uint job_base = add_word(pointer_at(config, 5u), 1u, job_count);
        for (uint chunk = 0u; chunk < job_count; ++chunk) {
            uint remaining = partition_count - chunk * 128u;
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u, partition_start + chunk * 128u);
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
            write_word(pointer_at(config, 11u), (job_base + chunk) * 3u + 2u, group * 512u + lane * 128u);
        }
    }
    BARRIER();
    if (!retained) {
        uint job_count = (count + 127u) / 128u;
        if (lane == 0u) tile_job_base = add_word(pointer_at(config, 5u), 0u, job_count);
        BARRIER();
        for (uint chunk = lane; chunk < job_count; chunk += group_width()) {
            uint remaining = count - chunk * 128u;
            write_word(pointer_at(config, 9u), (tile_job_base + chunk) * 3u, start + chunk * 128u);
            write_word(pointer_at(config, 9u), (tile_job_base + chunk) * 3u + 1u, remaining < 128u ? remaining : 128u);
            write_word(pointer_at(config, 9u), (tile_job_base + chunk) * 3u + 2u, group);
        }
        return;
    }
    UNROLL for (uint slot = 0u; slot < LOCAL_TILE_SLOTS; ++slot) {
        uint index = lane + slot * group_width();
        if (index < count) {
            uint relative = values[slot];
            uint bucket = relative / 15113u;
#ifdef __CUDACC__
            uint destination = atomicAdd(&tile_counts[bucket], 1u);
#else
            uint destination = atomicAdd(tile_counts[bucket], 1u);
#endif
            tile_records[destination] = relative % (128u * 15113u);
            tile_records[destination + 4096u] = sources[slot];
        }
    }
    BARRIER();
    for (uint index = lane; index < count; index += group_width()) {
        write_pair(pointer_at(config, 6u), start + index, tile_records[index], tile_records[index + 4096u]);
    }
#undef LOCAL_TILE_SLOTS
}
DEVICE void scatter_compact(Config config, uint group, uint lane) {
    if (parameter(config, 6u) == 5u) {
        if (group >= parameter(config, 1u) || lane >= read_word(pointer_at(config, 9u), group * 3u + 1u)) return;
        uint source = read_word(pointer_at(config, 9u), group * 3u) + lane;
        bool first_table = parameter(config, 2u) == 1u;
        Address value = first_table || parameter(config, 5u) == 0u
            ? read_pair(pointer_at(config, 8u), source * (first_table ? 1u : 2u))
            : partitioned_second_value_at(config, pointer_at(config, 8u), source, group);
        uint first = uint(value);
        uint second = uint(value >> 32u);
        uint bucket = first_table ? first_bucket(first, second) : uint(value / Address(15113u));
        uint index = add_word(pointer_at(config, 2u), bucket, 0xffffffffu) - 1u;
        write_pair(pointer_at(config, 6u), index, first_table ? first : uint(value % Address(128u * 15113u)), first_table ? second : source);
        return;
    }
    if (parameter(config, 6u) == 4u && read_word(pointer_at(config, 5u), 2u) != 0u) return;
    if (group >= read_word(pointer_at(config, 5u), parameter(config, 6u) == 4u ? 0u : 1u)) return;
    uint start = read_word(pointer_at(config, 9u), group * 3u);
    uint count = read_word(pointer_at(config, 9u), group * 3u + 1u);
    if (lane >= count) return;
    uint source = start + lane;
    Address pair = read_pair(pointer_at(config, 8u), source);
    uint first = uint(pair);
    uint second = uint(pair >> 32u);
    uint coarse = read_word(pointer_at(config, 9u), group * 3u + 2u);
    uint relative = parameter(config, 2u) == 1u
        ? uint((Address(first) << 6u | Address(second >> 26u)) - (Address(coarse) << parameter(config, 7u)) * Address(15113u))
        : first;
    uint bucket = (coarse << parameter(config, 7u)) + relative / 15113u;
    uint index = add_word(pointer_at(config, 2u), bucket, 0xffffffffu) - 1u;
    if (parameter(config, 2u) == 1u) {
        write_pair(pointer_at(config, 6u), index, first, second);
    } else {
        write_pair(pointer_at(config, 6u), index, relative % (128u * 15113u), second);
    }
}
DEVICE void hash_second_partitioned(Config config, uint group, uint lane) {
#ifdef __CUDACC__
    __shared__ uint second_partition_counts[64], second_partition_bases[64], second_partition_records[3072];
#endif
    if (lane < 64u) second_partition_counts[lane] = 0u;
    BARRIER();
    uint source = parameter(config, 4u) + group * group_width() + lane;
    bool valid = source < parameter(config, 1u);
    uint node[8];
    uint region = 0u;
    uint local_index = 0u;
    if (valid) {
        Address input_nodes = pointer_at(config, 0u);
        uint input_row[4];
        read_quad(input_nodes, source, input_row);
        evaluate_second(Address(input_row[0]) | (Address(input_row[1]) << 32u), input_row[2], input_row[3], node);
        region = node[1];
        if (pointer_at(config, 3u) != Address(0u)) {
            Address value = Address(node[0]) | Address(node[1]) << 32u;
            uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
            add_word(pointer_at(config, 3u), coarse, 1u);
        }
#ifdef __CUDACC__
        local_index = atomicAdd(&second_partition_counts[region], 1u);
#else
        local_index = atomicAdd(second_partition_counts[region], 1u);
#endif
        if (local_index < 16u) {
            uint slot = (region * 16u + local_index) * 3u;
            second_partition_records[slot] = node[0];
            second_partition_records[slot + 1u] = node[2];
            second_partition_records[slot + 2u] = node[3];
        }
    }
    BARRIER();
    Address ranges = pointer_at(config, 9u);
    if (lane < 64u) {
        uint count = second_partition_counts[lane];
        uint base = count == 0u ? 0u : add_word(ranges, lane * 3u + 2u, count);
        second_partition_bases[lane] = base;
        uint capacity = read_word(ranges, lane * 3u + 1u);
        if (base > capacity || count > capacity - base) exchange_word(pointer_at(config, 5u), 2u, 4u);
    }
    BARRIER();
    Address output_nodes = pointer_at(config, 1u);
    for (uint work = lane; work < 1024u; work += group_width()) {
        uint output_region = work / 16u;
        uint output_slot = work % 16u;
        uint count = second_partition_counts[output_region];
        uint offset = second_partition_bases[output_region] + output_slot;
        if (count <= 16u && output_slot < count && offset < read_word(ranges, output_region * 3u + 1u)) {
            uint destination = read_word(ranges, output_region * 3u) + offset;
            uint record = work * 3u;
            write_partitioned_second(config, output_nodes, destination, second_partition_records[record], output_region, second_partition_records[record + 1u], second_partition_records[record + 2u]);
        }
    }
    if (valid && second_partition_counts[region] > 16u) {
        uint offset = second_partition_bases[region] + local_index;
        if (offset < read_word(ranges, region * 3u + 1u)) {
            uint destination = read_word(ranges, region * 3u) + offset;
            write_partitioned_second(config, output_nodes, destination, node[0], node[1], node[2], node[3]);
        }
    }
}
DEVICE void scatter_partitioned_coarse(Config config, uint group, uint lane) {
#ifdef __CUDACC__
    __shared__ uint coarse_counts[576], coarse_bases[576], coarse_records[10368];
#endif
    uint slot_bits = parameter(config, 7u) == 9u ? 3u : 4u;
    uint slots = 1u << slot_bits;
    uint bins = slot_bits == 3u ? 576u : 288u;
    uint stride = slots + 1u;
    uint plane_words = bins * stride;
    uint first_job = group * 16u;
    if (first_job >= parameter(config, 1u)) return;
    Address jobs = pointer_at(config, 9u);
    uint region = read_word(jobs, first_job * 3u + 2u);
    uint coarse_base = uint((Address(region) << 32u) >> parameter(config, 7u)) / 15113u;
    for (uint bucket = lane; bucket < bins; bucket += group_width()) coarse_counts[bucket] = 0u;
    BARRIER();
    for (uint work = lane; work < 2048u; work += group_width()) {
        uint job = first_job + work / 128u;
        uint local_row = work % 128u;
        if (job >= parameter(config, 1u) || local_row >= read_word(jobs, job * 3u + 1u)) continue;
        uint source = read_word(jobs, job * 3u) + local_row;
        Address value = partitioned_second_value_at(config, pointer_at(config, 0u), source, job);
        uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
        uint relative = uint(value - (Address(coarse) << parameter(config, 7u)) * Address(15113u));
        uint bucket = coarse - coarse_base;
        if (coarse >= coarse_base && bucket < bins) {
#ifdef __CUDACC__
            uint slot = atomicAdd(&coarse_counts[bucket], 1u);
#else
            uint slot = atomicAdd(coarse_counts[bucket], 1u);
#endif
            if (slot < slots) {
                uint record = bucket * stride + slot;
                coarse_records[record] = relative;
                coarse_records[record + plane_words] = source;
                continue;
            }
        }
        uint destination = add_word(pointer_at(config, 7u), coarse * 2u, 0xffffffffu) - 1u;
        write_pair(pointer_at(config, 8u), destination, relative, source);
    }
    BARRIER();
    for (uint bucket = lane; bucket < bins; bucket += group_width()) {
        uint count = coarse_counts[bucket] < slots ? coarse_counts[bucket] : slots;
        coarse_bases[bucket] = count == 0u ? 0u
            : add_word(pointer_at(config, 7u), (coarse_base + bucket) * 2u, 0u - count) - count;
    }
    BARRIER();
    for (uint work = lane; work < 4608u; work += group_width()) {
        uint bucket = work >> slot_bits;
        uint slot = work & (slots - 1u);
        if (slot >= coarse_counts[bucket]) continue;
        uint destination = coarse_bases[bucket] + slot;
        uint record = bucket * stride + slot;
        write_pair(pointer_at(config, 8u), destination, coarse_records[record], coarse_records[record + plane_words]);
    }
}
#ifdef __CUDACC__
template <uint operation>
DEVICE __forceinline__ void run(Config config, uint group, uint lane) {
#else
DEVICE void run(Config config, uint group, uint lane) {
    uint operation = operation_kind;
#endif
    uint invocation = group * 128u + lane;
    if (operation == 0u && invocation < parameter(config, 1u)) write_word(pointer_at(config, 2u), invocation, 0xffffffffu);
    if (operation == 1u) generate(config, group * group_width() + lane, lane);
#ifdef __CUDACC__
    if (operation == 18u) generate_partitioned<false>(config, group * group_width() + lane, lane);
    if (operation == 21u) generate_partitioned<true>(config, group * group_width() + lane, lane);
#else
    if (operation == 18u || operation == 21u) generate_partitioned(config, group * group_width() + lane, lane, operation == 21u);
#endif
    if (operation == 17u) hash_second_partitioned(config, group, lane);
    if (operation == 19u) scatter_partitioned_coarse(config, group, lane);
    if (operation == 2u) index_nodes(config, invocation);
    if (operation == 3u) match_nodes(config, group, lane);
    if (operation == 4u) gather_fifth(config, invocation);
    if (operation == 5u) gather_proofs(config, invocation);
    if (operation == 7u) match_sparse(config, group, lane, parameter(config, 2u));
    if (operation == 15u) match_sparse(config, group, lane, 3u);
    if (operation == 13u) coarse_offsets(config, group, lane);
    if (operation == 14u) {
        uint source = invocation;
        if (parameter(config, 6u) == 6u) {
            if (group >= parameter(config, 1u) || lane >= read_word(pointer_at(config, 9u), group * 3u + 1u)) return;
            source = read_word(pointer_at(config, 9u), group * 3u) + lane;
        } else if (invocation >= parameter(config, 1u)) return;
        Address value = parameter(config, 6u) == 6u
            ? partitioned_second_value_at(config, pointer_at(config, 0u), source, group)
            : value_at(pointer_at(config, 0u), source, parameter(config, 2u) == 1u ? 2u : 4u);
        uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
        uint index = add_word(pointer_at(config, 7u), coarse * 2u, 0xffffffffu) - 1u;
        if (parameter(config, 2u) == 1u) {
            write_word(pointer_at(config, 8u), index * 2u, uint(value >> 6u));
            write_word(pointer_at(config, 8u), index * 2u + 1u, read_word(pointer_at(config, 0u), source * 2u + 1u));
        } else {
            write_word(pointer_at(config, 8u), index * 2u, uint(value - (Address(coarse) << parameter(config, 7u)) * Address(15113u)));
            write_word(pointer_at(config, 8u), index * 2u + 1u, source);
        }
    }
    if (operation == 9u && invocation < parameter(config, 1u)) {
        write_word(pointer_at(config, 2u), invocation, 0xffffffffu);
        uint count = parameter(config, 6u) == 2u ? read_word(pointer_at(config, 3u), invocation) & 65535u : 0u;
        write_word(pointer_at(config, 3u), invocation, count);
        if (parameter(config, 6u) == 2u && parameter(config, 2u) == 1u && count != 0u) {
            uint active_index = add_word(pointer_at(config, 5u), 1u, 1u);
            write_word(pointer_at(config, 4u), active_index, invocation);
        }
    }
    if (operation == 10u && parameter(config, 6u) == 6u) {
        if (group >= parameter(config, 1u) || lane >= read_word(pointer_at(config, 9u), group * 3u + 1u)) return;
        uint source = read_word(pointer_at(config, 9u), group * 3u) + lane;
        Address value = partitioned_second_value_at(config, pointer_at(config, 0u), source, group);
        if (value >= (Address(1u) << 38u)) { exchange_word(pointer_at(config, 5u), 2u, 3u); return; }
        uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
        add_word(pointer_at(config, 11u), coarse, 1u);
        return;
    }
    if (operation == 10u && parameter(config, 6u) == 5u) {
        if (group >= parameter(config, 1u) || lane >= read_word(pointer_at(config, 9u), group * 3u + 1u)) return;
        uint source = read_word(pointer_at(config, 9u), group * 3u) + lane;
        bool first_table = parameter(config, 2u) == 1u;
        uint bucket;
        if (first_table) {
            bucket = first_bucket(read_word(pointer_at(config, 0u), source * 2u), read_word(pointer_at(config, 0u), source * 2u + 1u));
        } else {
            bucket = uint(partitioned_second_value_at(config, pointer_at(config, 0u), source, group) / Address(15113u));
        }
        if (bucket >= 18188177u) { exchange_word(pointer_at(config, 5u), 2u, 3u); return; }
        uint previous = add_word(pointer_at(config, 3u), bucket, 1u);
        if (previous == 0u && first_table) {
            uint active_index = add_word(pointer_at(config, 5u), 1u, 1u);
            write_word(pointer_at(config, 4u), active_index, bucket);
        }
        if (previous >= 1024u) exchange_word(pointer_at(config, 5u), 2u, 2u);
        return;
    }
    if (operation == 10u && invocation < parameter(config, 1u)) {
        Address value = value_at(pointer_at(config, 0u), invocation, parameter(config, 2u) == 1u ? 2u : 4u);
        if (parameter(config, 6u) == 3u) {
            if (value >= (Address(1u) << 38u)) { exchange_word(pointer_at(config, 5u), 2u, 3u); return; }
            uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
            add_word(pointer_at(config, 9u), coarse, 1u);
            return;
        }
        uint bucket = uint(value / Address(15113u));
        if (bucket >= 18188177u) { exchange_word(pointer_at(config, 5u), 2u, 3u); return; }
        uint previous = add_word(pointer_at(config, 3u), bucket, 1u);
        if (previous == 0u && parameter(config, 2u) == 1u) {
            uint active_index = add_word(pointer_at(config, 5u), 1u, 1u);
            write_word(pointer_at(config, 4u), active_index, bucket);
        }
        if (previous >= 1024u) exchange_word(pointer_at(config, 5u), 2u, 2u);
    }
    if (operation == 11u && parameter(config, 2u) == 2u) bucket_offsets(config, invocation, lane);
    if (operation == 11u && parameter(config, 2u) == 1u && invocation < parameter(config, 1u)) {
        uint bucket = read_word(pointer_at(config, 4u), invocation);
        uint count = read_word(pointer_at(config, 3u), bucket);
        uint offset = add_word(pointer_at(config, 5u), 3u, count);
        write_word(pointer_at(config, 2u), bucket, offset + count);
    }
    if (operation == 12u) scatter_compact(config, group, lane);
    if (operation == 16u) local_compact(config, group, lane);
    if (operation == 20u) local_compact_coalesced(config, group, lane);
    if (operation == 8u) {
        uint index = invocation + parameter(config, 4u);
        if (index < parameter(config, 1u)) {
            Address nodes = pointer_at(config, 1u);
            uint node[8];
            if (parameter(config, 2u) == 2u) {
                evaluate_second(value_at(nodes, index, 4u), read_word(nodes, index * 4u + 2u), read_word(nodes, index * 4u + 3u), node);
                write_word(nodes, index * 4u, node[0]);
                write_word(nodes, index * 4u + 1u, node[1]);
                if (pointer_at(config, 9u) != Address(0u)) {
                    Address value = Address(node[0]) | Address(node[1]) << 32u;
                    if (parameter(config, 6u) == 3u) {
                        uint coarse = uint(value >> parameter(config, 7u)) / 15113u;
                        add_word(pointer_at(config, 9u), coarse, 1u);
                    } else {
                        uint bucket = uint(value / Address(15113u));
                        if (bucket >= 18188177u) exchange_word(pointer_at(config, 5u), 2u, 3u);
                        else if (add_word(pointer_at(config, 9u), bucket, 1u) >= 1024u) exchange_word(pointer_at(config, 5u), 2u, 2u);
                    }
                }
            } else {
                evaluate(config, read_word(nodes, index * 8u + 6u), read_word(nodes, index * 8u + 7u), 3u, node);
                for (uint word = 0u; word < 6u; ++word) write_word(nodes, index * 8u + word, node[word]);
            }
        }
    }
    if (operation == 6u && invocation < parameter(config, 1u)) {
        uint node[8]; evaluate(config, invocation * 2u, invocation * 2u + 1u, parameter(config, 2u), node);
        for (uint index = 0u; index < 8u; ++index) write_word(pointer_at(config, 1u), invocation * 8u + index, node[index]);
    }
}
#ifdef __CUDACC__
#define GH_KERNEL(operation) extern "C" __global__ void gh_kernel_##operation(const KernelParameters config) { run<operation>(config, blockIdx.x, threadIdx.x); }
GH_KERNEL(0)
GH_KERNEL(1)
GH_KERNEL(2)
GH_KERNEL(3)
GH_KERNEL(4)
GH_KERNEL(5)
GH_KERNEL(6)
GH_KERNEL(7)
GH_KERNEL(8)
GH_KERNEL(9)
GH_KERNEL(10)
GH_KERNEL(11)
GH_KERNEL(12)
GH_KERNEL(13)
GH_KERNEL(14)
GH_KERNEL(15)
GH_KERNEL(16)
GH_KERNEL(17)
GH_KERNEL(18)
GH_KERNEL(19)
GH_KERNEL(20)
GH_KERNEL(21)
#undef GH_KERNEL
#else
void main() { run(Address(0), gl_WorkGroupID.x + gl_WorkGroupID.y * 65535u, gl_LocalInvocationID.x); }
#endif
