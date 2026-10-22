// Offline research adapter for the pinned meshoptimizer clusterlod example.
// The JSON output is an experiment artifact, not a runtime payload ABI.
#include "meshoptimizer.h"
#define CLUSTERLOD_IMPLEMENTATION
#include "clusterlod.h"

#include <chrono>
#include <cmath>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <locale>
#include <stdexcept>

static const char* pin = "9e1f07b159d3cb777f1c67ed31fc11fd117986f4";

static uint32_t readU32(std::istream& input)
{
    unsigned char bytes[4];
    if (!input.read(reinterpret_cast<char*>(bytes), 4))
        throw std::runtime_error("truncated input");
    return uint32_t(bytes[0]) | uint32_t(bytes[1]) << 8 | uint32_t(bytes[2]) << 16 | uint32_t(bytes[3]) << 24;
}

static float readFloat(std::istream& input)
{
    uint32_t bits = readU32(input);
    float value;
    memcpy(&value, &bits, sizeof(value));
    if (!std::isfinite(value))
        throw std::runtime_error("non-finite input value");
    return value;
}

static void indicesJson(const std::vector<unsigned int>& indices)
{
    std::cout << '[';
    for (size_t i = 0; i < indices.size(); ++i)
        std::cout << (i ? "," : "") << indices[i];
    std::cout << ']';
}

static void boundsJson(const clodBounds& bounds, bool terminal = false)
{
    for (float value : bounds.center)
        if (!std::isfinite(value))
            throw std::runtime_error("non-finite output bounds");
    if (!std::isfinite(bounds.radius) || bounds.radius < 0 || !std::isfinite(bounds.error) || bounds.error < 0)
        throw std::runtime_error("invalid output bounds/error");
    std::cout << "{\"center\":[" << bounds.center[0] << ',' << bounds.center[1] << ',' << bounds.center[2]
              << "],\"radius\":" << bounds.radius << ",\"error_estimate\":";
    if (terminal)
        std::cout << "null";
    else
        std::cout << bounds.error;
    std::cout << '}';
}

struct SavedCluster
{
    int refined;
    clodBounds bounds;
    std::vector<unsigned int> indices;
};

struct SavedGroup
{
    clodGroup group;
    std::vector<SavedCluster> clusters;
};

struct Conventional
{
    float ratio;
    float error;
    std::vector<unsigned int> indices;
};

int main(int argc, char** argv)
{
    try
    {
        if (argc != 3)
            throw std::runtime_error("usage: builder INPUT.bin MAX_CLUSTER_TRIANGLES");
        std::cout.imbue(std::locale::classic());
        std::cerr.imbue(std::locale::classic());
        std::cout << std::setprecision(std::numeric_limits<float>::max_digits10);
        std::cerr << std::setprecision(12);

        std::string triangles_arg(argv[2]);
        size_t parsed = 0;
        unsigned long max_triangles = std::stoul(triangles_arg, &parsed);
        if (parsed != triangles_arg.size() || max_triangles < 4 || max_triangles > 256)
            throw std::runtime_error("max cluster triangles must be in 4..256");

        std::ifstream input(argv[1], std::ios::binary);
        char magic[8];
        if (!input.read(magic, 8) || memcmp(magic, "MCLODX01", 8) != 0)
            throw std::runtime_error("invalid experiment input magic");
        uint32_t vertex_count = readU32(input), index_count = readU32(input), attribute_count = readU32(input);
        if (!vertex_count || vertex_count > 1000000 || !index_count || index_count > 6000000 || index_count % 3 || attribute_count > 32)
            throw std::runtime_error("input counts exceed experiment limits");

        std::vector<float> positions(size_t(vertex_count) * 3);
        std::vector<float> attributes(size_t(vertex_count) * attribute_count);
        std::vector<float> weights(attribute_count);
        std::vector<unsigned char> locks(vertex_count);
        std::vector<unsigned int> indices(index_count);
        for (float& value : positions)
            value = readFloat(input);
        for (float& value : attributes)
            value = readFloat(input);
        for (float& value : weights)
        {
            value = readFloat(input);
            if (value < 0)
                throw std::runtime_error("negative attribute weight");
        }
        if (!input.read(reinterpret_cast<char*>(locks.data()), locks.size()))
            throw std::runtime_error("truncated vertex locks");
        for (unsigned char value : locks)
            if (value > 1)
                throw std::runtime_error("unsupported vertex lock flags");
        for (unsigned int& value : indices)
        {
            value = readU32(input);
            if (value >= vertex_count)
                throw std::runtime_error("index outside source vertex buffer");
        }
        if (input.peek() != std::char_traits<char>::eof())
            throw std::runtime_error("unexpected input tail");

        using Clock = std::chrono::steady_clock;
        auto conventional_started = Clock::now();
        std::vector<Conventional> conventional;
        for (float ratio : {0.5f, 0.25f, 0.125f})
        {
            size_t target = std::max(size_t(3), size_t(double(index_count / 3) * ratio) * 3);
            Conventional candidate{ratio, 0.f, std::vector<unsigned int>(index_count)};
            size_t count = meshopt_simplifyWithAttributes(candidate.indices.data(), indices.data(), indices.size(),
                positions.data(), vertex_count, 12, attributes.empty() ? nullptr : attributes.data(), attribute_count * 4,
                weights.empty() ? nullptr : weights.data(), attribute_count, locks.data(), target, FLT_MAX,
                meshopt_SimplifyErrorAbsolute | meshopt_SimplifyPreserveFolds, &candidate.error);
            if (!count || count % 3 || !std::isfinite(candidate.error) || candidate.error < 0)
                throw std::runtime_error("invalid conventional output");
            candidate.indices.resize(count);
            conventional.push_back(std::move(candidate));
        }
        auto conventional_finished = Clock::now();

        clodConfig config = clodDefaultConfig(max_triangles);
        // Keep source vertices immutable and avoid attribute-discontinuity/sloppy
        // fallbacks. Both candidates receive the same source open-border locks.
        config.simplify_permissive = false;
        config.simplify_fallback_permissive = false;
        config.simplify_fallback_sloppy = false;
        config.simplify_error_clamped = false;
        config.simplify_preserve_folds = true;
        config.simplify_dilate_borders = false;

        clodMesh mesh = {};
        mesh.indices = indices.data();
        mesh.index_count = indices.size();
        mesh.vertex_count = vertex_count;
        mesh.vertex_positions = positions.data();
        mesh.vertex_positions_stride = 12;
        mesh.vertex_attributes = attributes.empty() ? nullptr : attributes.data();
        mesh.vertex_attributes_stride = attribute_count * 4;
        mesh.attribute_weights = weights.empty() ? nullptr : weights.data();
        mesh.attribute_count = attribute_count;
        mesh.vertex_lock = locks.data();

        std::vector<SavedGroup> groups;
        auto cluster_started = Clock::now();
        clodBuild(config, mesh, [&](clodGroup group, const clodCluster* clusters, size_t count) -> int {
            SavedGroup saved{group, {}};
            for (size_t i = 0; i < count; ++i)
                saved.clusters.push_back({clusters[i].refined, clusters[i].bounds,
                    std::vector<unsigned int>(clusters[i].indices, clusters[i].indices + clusters[i].index_count)});
            groups.push_back(std::move(saved));
            return int(groups.size() - 1);
        });
        auto cluster_finished = Clock::now();

        std::cout << "{\"meshoptimizer_commit\":\"" << pin << "\",\"error_semantics\":\"meshoptimizer heuristic; not certified conservative\","
                  << "\"conventional\":[";
        for (size_t i = 0; i < conventional.size(); ++i)
        {
            const Conventional& candidate = conventional[i];
            std::cout << (i ? "," : "") << "{\"target_ratio\":" << candidate.ratio << ",\"error_estimate\":" << candidate.error << ",\"indices\":";
            indicesJson(candidate.indices);
            std::cout << '}';
        }
        std::cout << "],\"groups\":[";
        for (size_t i = 0; i < groups.size(); ++i)
        {
            const SavedGroup& saved = groups[i];
            bool terminal = saved.group.simplified.error == FLT_MAX;
            std::cout << (i ? "," : "") << "{\"id\":" << i << ",\"depth\":" << saved.group.depth
                      << ",\"terminal\":" << (terminal ? "true" : "false") << ",\"simplified_bounds\":";
            boundsJson(saved.group.simplified, terminal);
            std::cout << ",\"clusters\":[";
            for (size_t j = 0; j < saved.clusters.size(); ++j)
            {
                const SavedCluster& cluster = saved.clusters[j];
                std::cout << (j ? "," : "") << "{\"refines_group\":" << cluster.refined << ",\"bounds\":";
                boundsJson(cluster.bounds);
                std::cout << ",\"indices\":";
                indicesJson(cluster.indices);
                std::cout << '}';
            }
            std::cout << "]}";
        }
        std::cout << "]}\n";
        std::cerr << "{\"conventional_seconds\":" << std::chrono::duration<double>(conventional_finished - conventional_started).count()
                  << ",\"cluster_seconds\":" << std::chrono::duration<double>(cluster_finished - cluster_started).count() << "}\n";
        return 0;
    }
    catch (const std::exception& error)
    {
        std::cerr << "builder: " << error.what() << '\n';
        return 2;
    }
}
