// Inspect completed RocksDB 8.10 format-v5 SSTs from Stem's load fixture.
// The data-block sizes and entry counts are cross-checked against RocksDB's LOG.
#include <snappy.h>

#include <cstdint>
#include <fstream>
#include <iostream>
#include <iterator>
#include <map>
#include <stdexcept>
#include <string>
#include <string_view>

using View = std::string_view;

void require(bool valid, const char* message) {
    if (!valid) throw std::runtime_error(message);
}

View take(View& input, size_t count) {
    require(count <= input.size(), "truncated field");
    View value = input.substr(0, count);
    input.remove_prefix(count);
    return value;
}

uint64_t little(View bytes) {
    uint64_t value = 0;
    require(bytes.size() <= 8, "oversized integer");
    for (size_t i = 0; i < bytes.size(); ++i)
        value |= uint64_t(uint8_t(bytes[i])) << (8 * i);
    return value;
}

uint64_t varint(View& bytes) {
    uint64_t value = 0;
    for (unsigned shift = 0; shift < 64; shift += 7) {
        uint8_t next = uint8_t(take(bytes, 1)[0]);
        require(shift != 63 || next <= 1, "invalid varint");
        value |= uint64_t(next & 127) << shift;
        if (!(next & 128)) return value;
    }
    throw std::runtime_error("unterminated varint");
}

View entries(View block) {
    require(block.size() >= 4, "missing restart count");
    auto restarts = little(block.substr(block.size() - 4));
    require(restarts && restarts <= (block.size() - 4) / 4, "unsupported restart array");
    size_t end = block.size() - 4 * (restarts + 1);
    require(little(block.substr(end, 4)) == 0, "invalid first restart");
    return block.substr(0, end);
}

std::string unpack(View file, size_t offset, size_t length) {
    require(offset < file.size() && length < file.size() - offset, "block outside SST");
    auto raw = file.substr(offset, length);
    const uint8_t codec = file[offset + length];
    if (codec == 0) return std::string(raw);
    require(codec == 1, "unsupported compression");
    std::string decoded;
    require(snappy::Uncompress(raw.data(), raw.size(), &decoded), "invalid Snappy block");
    return decoded;
}

View category(View key) {
    if (key.starts_with("jbody")) return "proposal_journal";
    if (key.starts_with("j")) return "consensus_records";
    if (key.starts_with("e")) return "execution";
    if (key.starts_with("h") || key.starts_with("i") || key == View("\0stem/history", 13))
        return "headers_and_history";
    if (key.starts_with(View("s\0", 2))) return "live_cells";
    if (key.starts_with(View("s\1", 2))) return "system_records";
    if (key.starts_with(View("n\0", 2))) return "cell_trie";
    if (key.starts_with(View("n\1", 2))) return "system_trie";
    return "other";
}

struct Totals {
    size_t bytes = 0, blocks = 0, entries = 0;
};

void inspect(const char* path) {
    std::ifstream input(path, std::ios::binary);
    require(input.good(), "cannot open SST");
    std::string data{std::istreambuf_iterator<char>(input), {}};
    View file = data;
    require(file.size() >= 53, "incomplete footer");
    View footer = file.substr(file.size() - 53);
    require(little(footer.substr(41, 4)) == 5 &&
            little(footer.substr(45, 8)) == 0x88e241b785f4cff7ULL, "unsupported SST format");
    footer.remove_prefix(1);
    varint(footer); varint(footer);  // metaindex block handle
    auto index_offset = varint(footer);
    auto index_size = varint(footer);
    auto index = unpack(file, index_offset, index_size);
    View records = entries(index);
    uint64_t offset = 0, size = 0, next = 0;
    std::map<View, Totals> result;
    while (!records.empty()) {
        auto shared = varint(records);
        auto suffix = varint(records);
        take(records, suffix);
        if (shared) {
            auto encoded = varint(records);
            auto difference = int64_t(encoded >> 1) ^ -int64_t(encoded & 1);
            require(difference >= -int64_t(size) && difference <= int64_t(file.size()), "invalid index size delta");
            size = uint64_t(int64_t(size) + difference);
            offset = next;
        } else {
            offset = varint(records);
            size = varint(records);
        }
        require(offset == next && size <= index_offset && offset + size + 5 <= index_offset, "discontiguous data block");
        next = offset + size + 5;
        auto block = unpack(file, offset, size);
        auto rows = entries(block);
        std::string key;
        View namespace_name;
        size_t count = 0;
        while (!rows.empty()) {
            auto prefix = varint(rows);
            auto suffix_size = varint(rows);
            auto value_size = varint(rows);
            require(prefix <= key.size(), "invalid shared key prefix");
            key.resize(prefix);
            key.append(take(rows, suffix_size));
            take(rows, value_size);
            require(key.size() >= 8, "short RocksDB internal key");
            View type = category(View(key).substr(0, key.size() - 8));
            if (namespace_name.empty()) namespace_name = type;
            else if (namespace_name != type) namespace_name = "mixed";
            ++count;
        }
        require(count > 0, "empty data block");
        auto& totals = result[namespace_name];
        totals.bytes += size + 5;
        ++totals.blocks;
        totals.entries += count;
    }
    result["metadata"].bytes = file.size() - next;
    for (const auto& [name, totals] : result)
        std::cout << name << '\t' << totals.bytes << '\t' << totals.blocks << '\t' << totals.entries << '\n';
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    try { inspect(argv[1]); }
    catch (const std::exception& error) {
        std::cerr << argv[1] << ": " << error.what() << '\n';
        return 1;
    }
}
