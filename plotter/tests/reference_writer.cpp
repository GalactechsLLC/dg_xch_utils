#include "plot/Plotter.hpp"
#include "plot/PlotFile.hpp"
#include "pos/ProofValidator.hpp"
#include "common/Utils.hpp"
#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <system_error>
#include <thread>
#include <fcntl.h>
#ifdef _WIN32
#include <io.h>
#else
#include <unistd.h>
#endif

struct BenchmarkProgress final : IProgressSink {
    std::array<uint64_t, 4> table_counts{};

    bool on_event(ProgressEvent const& event) noexcept override {
        if (event.kind == EventKind::TableBegin) {
            std::fprintf(stderr, "reference_table_begin table=%u inputs=%llu\n",
                unsigned(event.table_id), static_cast<unsigned long long>(event.num_items_in));
        } else if (event.kind == EventKind::TableEnd || event.kind == EventKind::PostSortEnd
            || event.kind == EventKind::AllocationEnd) {
            auto name = event.kind == EventKind::TableEnd ? "table"
                : event.kind == EventKind::PostSortEnd ? "sort" : "allocation";
            std::fprintf(stderr, "reference_phase phase=%s table=%u seconds=%.6f\n", name,
                unsigned(event.table_id), static_cast<double>(event.elapsed) / 1'000'000'000.0);
            if (event.kind == EventKind::PostSortEnd && event.table_id < table_counts.size()) {
                table_counts[event.table_id] = event.produced;
            }
        } else if (event.kind == EventKind::Note) {
            if (event.note_id == NoteId::LayoutTotalBytesAllocated) {
                std::fprintf(stderr, "reference_layout_bytes=%llu\n",
                    static_cast<unsigned long long>(event.u64_0));
            } else if (event.note_id == NoteId::HasAESHardware) {
                std::fprintf(stderr, "reference_hardware_aes=%llu\n",
                    static_cast<unsigned long long>(event.u64_0));
            }
        }
        return true;
    }
};

void syncPlotFile(std::filesystem::path const& path) {
#ifdef _WIN32
    auto descriptor = _wopen(path.c_str(), _O_RDWR | _O_BINARY);
    if (descriptor < 0) throw std::system_error(errno, std::generic_category(), "open plot for sync");
    auto result = _commit(descriptor);
    auto saved_errno = errno;
    _close(descriptor);
    if (result != 0) throw std::system_error(saved_errno, std::generic_category(), "sync plot");
#else
    auto sync_path = [](std::filesystem::path const& target) {
        auto descriptor = open(target.c_str(), O_RDONLY);
        if (descriptor < 0) throw std::system_error(errno, std::generic_category(), "open for sync");
        auto result = fsync(descriptor);
        auto saved_errno = errno;
        close(descriptor);
        if (result != 0) throw std::system_error(saved_errno, std::generic_category(), "sync plot");
    };
    sync_path(path);
    auto parent = path.parent_path();
    sync_path(parent.empty() ? std::filesystem::path(".") : parent);
#endif
}

uint64_t readLittleEndian(std::istream& input, size_t width) {
    std::array<uint8_t, 8> bytes{};
    if (width > bytes.size()) throw std::invalid_argument("invalid integer width");
    input.read(reinterpret_cast<char*>(bytes.data()), width);
    if (!input) throw std::runtime_error("truncated plot metadata");
    uint64_t value = 0;
    for (size_t offset = 0; offset < width; ++offset) {
        value |= uint64_t(bytes[offset]) << (offset * 8);
    }
    return value;
}

int main(int argc, char** argv) {
    if (argc != 4 && argc != 6) return 2;
    try {
        std::ifstream input(argv[1], std::ios::binary);
        std::array<uint8_t, 43> header{};
        input.read(reinterpret_cast<char*>(header.data()), header.size());
        if (!input || header[42] > 128) return 2;
        std::vector<uint8_t> memo(header[42]);
        input.read(reinterpret_cast<char*>(memo.data()), memo.size());
        if (!input) return 2;
        ProofParams params(header.data() + 5, header[37], header[38], std::string(argv[3]) == "true");
        if (argc == 4 && std::string(argv[2]) == "scan") {
            if (!std::equal(header.begin(), header.begin() + 4, "pos2")
                || header[4] != PlotFile::FORMAT_VERSION || header[37] < 18
                || header[37] > 32 || header[37] % 2 != 0) {
                throw std::runtime_error("invalid format-one plot header");
            }
            auto chunk_count = readLittleEndian(input, 8);
            auto maximum_chunks = uint64_t(1) << (header[37] - PlotFile::CHUNK_SPAN_RANGE_BITS);
            if (chunk_count == 0 || chunk_count > maximum_chunks) {
                throw std::runtime_error("invalid plot chunk count");
            }
            std::vector<uint64_t> offsets;
            offsets.reserve(chunk_count);
            for (uint64_t chunk = 0; chunk < chunk_count; ++chunk) {
                offsets.push_back(readLittleEndian(input, 8));
            }
            auto data_start = static_cast<uint64_t>(input.tellg());
            input.seekg(0, std::ios::end);
            if (!input) throw std::runtime_error("could not determine plot file size");
            auto file_size = static_cast<uint64_t>(input.tellg());
            if (offsets.front() != data_start) {
                throw std::runtime_error("invalid first chunk offset");
            }
            auto span_bits = header[37] + PlotFile::CHUNK_SPAN_RANGE_BITS;
            PlotFile plot_file(argv[1]);
            uint64_t fragment_count = 0;
            uint64_t previous_fragment = 0;
            for (uint64_t chunk = 0; chunk < chunk_count; ++chunk) {
                auto end_offset = chunk + 1 < chunk_count ? offsets[chunk + 1] : file_size;
                if (offsets[chunk] > file_size || end_offset > file_size
                    || end_offset < offsets[chunk] || end_offset - offsets[chunk] < 20) {
                    throw std::runtime_error("invalid chunk offsets");
                }
                input.seekg(static_cast<std::streamoff>(offsets[chunk]));
                auto payload_size = readLittleEndian(input, 8);
                if (payload_size < 12 || payload_size > 64 * 1024 * 1024
                    || payload_size != end_offset - offsets[chunk] - 8) {
                    throw std::runtime_error("invalid bounded chunk payload size");
                }
                auto declared_fragments = readLittleEndian(input, 4);
                auto compressed_bytes = readLittleEndian(input, 4);
                auto stub_bytes = readLittleEndian(input, 4);
                if (declared_fragments > 1'000'000
                    || compressed_bytes + stub_bytes + 12 != payload_size
                    || stub_bytes != (declared_fragments * (header[37] - 2) + 7) / 8) {
                    throw std::runtime_error("invalid bounded chunk dimensions");
                }
                auto fragments = plot_file.readChunk(chunk);
                if (fragments.size() != declared_fragments) {
                    throw std::runtime_error("Chia decoded the wrong fragment count");
                }
                for (auto fragment : fragments) {
                    if ((fragment >> span_bits) != chunk
                        || (fragment_count != 0 && fragment < previous_fragment)) {
                        throw std::runtime_error("unsorted or out-of-range plot fragment");
                    }
                    previous_fragment = fragment;
                    ++fragment_count;
                }
                if (chunk + 1 == chunk_count && fragments.empty()) {
                    throw std::runtime_error("plot has an empty terminal chunk");
                }
            }
            std::cout << "Chia full-file scan passed: k=" << unsigned(header[37])
                      << " strength=" << unsigned(header[38]) << " chunks=" << chunk_count
                      << " fragments=" << fragment_count << " bytes=" << file_size << std::endl;
            return 0;
        }
        if (argc == 6) {
            if (std::string(argv[2]) != "verify" || std::string(argv[4]).size() != 64) return 2;
            auto challenge = Utils::hexToBytes(argv[4]);
            auto proof = Utils::compressedHexToKValues(header[37], argv[5]);
            if (proof.size() != TOTAL_XS_IN_PROOF) return 2;
            auto chain = ProofValidator(params).validate_full_proof(
                std::span<uint32_t const, TOTAL_XS_IN_PROOF>(proof.data(), proof.size()), challenge);
            if (!chain) {
                std::cerr << "Chia rejected the recovered full proof" << std::endl;
                return 1;
            }
            PlotFile plot_file(argv[1]);
            for (auto fragment : *chain) {
                auto fragments = plot_file.readChunk(fragment >> (header[37] + 16));
                if (std::find(fragments.begin(), fragments.end(), fragment) == fragments.end()) {
                    std::cerr << "Chia could not read fragment " << fragment << std::endl;
                    return 1;
                }
            }
            return 0;
        }
        auto benchmark = std::getenv("DGX_POS2_BENCHMARK") != nullptr;
        BenchmarkProgress progress;
        Plotter::Options options;
        if (benchmark) options.sink = &progress;
        auto started = std::chrono::steady_clock::now();
        auto plot = Plotter(params).run(options);
        auto build_finished = std::chrono::steady_clock::now();
        auto index = static_cast<uint16_t>(header[39] | (uint16_t(header[40]) << 8));
        auto bytes = PlotFile::writeData(argv[2], plot, params, index, header[41], memo);
        if (benchmark) {
            syncPlotFile(argv[2]);
            auto finished = std::chrono::steady_clock::now();
            std::cout << std::fixed << std::setprecision(6)
                      << "reference_benchmark k=" << unsigned(header[37])
                      << " strength=" << unsigned(header[38]) << " bytes=" << bytes
                      << " fragments=" << plot.t3_proof_fragments.size()
                      << " threads=" << std::thread::hardware_concurrency()
                      << " build_seconds=" << std::chrono::duration<double>(build_finished - started).count()
                      << " write_sync_seconds=" << std::chrono::duration<double>(finished - build_finished).count()
                      << " total_seconds=" << std::chrono::duration<double>(finished - started).count()
                      << " table_counts=" << progress.table_counts[0] << ',' << progress.table_counts[1]
                      << ',' << progress.table_counts[2] << ',' << progress.table_counts[3] << std::endl;
        }
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << std::endl;
        return 1;
    }
}
