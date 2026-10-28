/*
 * C wrapper over meshoptimizer's demo/clusterlod.h (MIT, v1.3). This file is ours, not part of meshoptimizer.
 * Rust reads the results through these plain structs; no callback crosses the FFI boundary.
 */
#ifndef MLC_WRAPPER_H
#define MLC_WRAPPER_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C"
{
#endif

typedef struct MlcBounds
{
	float center[3];
	float radius;
	float error; // FLT_MAX for terminal groups
} MlcBounds;

typedef struct MlcGroup
{
	int32_t depth;
	MlcBounds bounds; // simplified bounds of the group
	size_t cluster_begin; // range in mlc_clusters()
	size_t cluster_count;
} MlcGroup;

typedef struct MlcCluster
{
	int32_t refined; // index of the group that produced this cluster, or -1 for source geometry
	MlcBounds bounds;
	size_t index_begin; // range in mlc_indices()
	size_t index_count;
	size_t vertex_count;
} MlcCluster;

typedef struct MlcNode
{
	MlcBounds bounds;
	int32_t group; // leaf: group index; internal: -1
	uint32_t child_offset;
	uint32_t child_count;
} MlcNode;

typedef struct MlcMesh
{
	const unsigned int* indices;
	size_t index_count;
	const float* positions; // 3 floats per vertex
	size_t vertex_count;
	const float* attributes; // NULL when attribute_count is 0
	size_t attribute_stride; // bytes per vertex, multiple of 4
	const float* attribute_weights;
	size_t attribute_count;
	unsigned int attribute_protect_mask;
	const unsigned char* vertex_lock; // NULL, or one byte per vertex; non-zero locks the vertex in place
} MlcMesh;

typedef struct MlcResult MlcResult;

enum
{
	MLC_PRESET_CLASSIC = 0, // clodDefaultConfig
	MLC_PRESET_RT = 1, // clodDefaultConfigRT
};

// returns NULL on invalid arguments or allocation failure
// tail_min_triangles > 0 adds coarser single-cluster levels above each one-cluster root, down to
// about that many triangles (see mlc_wrapper.cpp, appendTail)
MlcResult* mlc_build(const MlcMesh* mesh, int preset, size_t max_triangles, size_t tail_min_triangles);
void mlc_free(MlcResult* result);

size_t mlc_group_count(const MlcResult* result);
const MlcGroup* mlc_groups(const MlcResult* result);
size_t mlc_cluster_count(const MlcResult* result);
const MlcCluster* mlc_clusters(const MlcResult* result);
size_t mlc_index_count(const MlcResult* result);
const unsigned int* mlc_indices(const MlcResult* result);

// upper bound on the node count of mlc_build_hierarchy
size_t mlc_hierarchy_bound(size_t group_count, size_t node_width, size_t level_count);
// returns the number of nodes written, or 0 on failure (capacity below the bound, a missing DAG level, bad width)
size_t mlc_build_hierarchy(MlcNode* nodes, size_t node_capacity, const MlcGroup* groups, size_t group_count, size_t node_width, size_t level_count);

// vertices and triangles need index_count entries each; returns the number of unique vertices
size_t mlc_local_indices(unsigned int* vertices, unsigned char* triangles, const unsigned int* indices, size_t index_count);

#ifdef __cplusplus
}
#endif

#endif
