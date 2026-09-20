#include "plot/Plotter.hpp"
#include "plot/PlotFile.hpp"
#include <array>
#include <fstream>
#include <iostream>

int main(int argc, char** argv) {
    if (argc != 4) return 2;
    try {
        std::ifstream input(argv[1], std::ios::binary);
        std::array<uint8_t, 43> header{};
        input.read(reinterpret_cast<char*>(header.data()), header.size());
        if (!input || header[42] > 128) return 2;
        std::vector<uint8_t> memo(header[42]);
        input.read(reinterpret_cast<char*>(memo.data()), memo.size());
        if (!input) return 2;
        ProofParams params(header.data() + 5, header[37], header[38], std::string(argv[3]) == "true");
        auto plot = Plotter(params).run();
        auto index = static_cast<uint16_t>(header[39] | (uint16_t(header[40]) << 8));
        PlotFile::writeData(argv[2], plot, params, index, header[41], memo);
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << std::endl;
        return 1;
    }
}
