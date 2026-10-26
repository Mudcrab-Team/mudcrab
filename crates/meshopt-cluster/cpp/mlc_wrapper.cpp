// C wrapper over meshoptimizer's demo/clusterlod.h (MIT, v1.3). This file is ours, not part of meshoptimizer.
#include "meshoptimizer.h"

#define CLUSTERLOD_IMPLEMENTATION
#include "clusterlod.h"

#include "mlc_wrapper.h"

#include <float.h>
#include <algorithm>
#include <string.h>

#include <memory>
#include <vector>

struct MlcResult
{
	std::vector<MlcGroup> groups;
	std::vector<MlcCluster> clusters;
	std::vector<unsigned int> indices;
};

static MlcBounds toMlcBounds(const clodBounds& b)
{
	MlcBounds r;
	memcpy(r.center, b.center, sizeof(r.center));
	r.radius = b.radius;
	r.error = b.error;
	return r;
}

static clodBounds toClodBounds(const MlcBounds& b)
{
	clodBounds r;
	memcpy(r.center, b.center, sizeof(r.center));
	r.radius = b.radius;
	r.error = b.error;
	return r;
}

// clodBuild calls this once per group, with the clusters that the group merges and simplifies.
// The returned id is stored as clodCluster::refined for the clusters this group produces.
static int collectGroup(void* context, clodGroup group, const clodCluster* clusters, size_t cluster_count)
{
	MlcResult& result = *static_cast<MlcResult*>(context);
	int id = int(result.groups.size());

	MlcGroup out = {};
	out.depth = group.depth;
	out.bounds = toMlcBounds(group.simplified);
	out.cluster_begin = result.clusters.size();
	out.cluster_count = cluster_count;
	result.groups.push_back(out);

	for (size_t i = 0; i < cluster_count; ++i)
	{
		const clodCluster& cluster = clusters[i];

		MlcCluster c = {};
		c.refined = cluster.refined;
		c.bounds = toMlcBounds(cluster.bounds);
		c.index_begin = result.indices.size();
		c.index_count = cluster.index_count;
		c.vertex_count = cluster.vertex_count;
		result.indices.insert(result.indices.end(), cluster.indices, cluster.indices + cluster.index_count);
		result.clusters.push_back(c);
	}

	return id;
}

// clodBuild stops when one cluster is left, so a mesh's root costs up to max_triangles for every
// instance however small it is on screen. This adds the levels below that size: for each terminal
// group that holds exactly one cluster, the cluster is simplified to about half, again and again,
// down to tail_min_triangles. Each step is one more group (the old terminal group, which gets the
// step's error instead of FLT_MAX) and one more cluster refined from it, under a new terminal
// group; the error rule is clodBuild's own (max(previous * merge_previous, error) + error *
// merge_additive), so errors stay monotonic and the selection rule needs no change. Stuck
// simplification (clodBuild's own threshold) ends the chain, leaving the last group terminal.
static void appendTail(MlcResult& result, const clodConfig& config, const clodMesh& mesh, size_t tail_min_triangles)
{
	std::vector<unsigned char> locks(mesh.vertex_count, 0);
	if (mesh.vertex_lock)
		locks.assign(mesh.vertex_lock, mesh.vertex_lock + mesh.vertex_count);
	std::vector<unsigned int> current, simplified, vertices;
	std::vector<unsigned char> triangles;

	size_t original = result.groups.size();
	for (size_t g = 0; g < original; ++g)
	{
		if (result.groups[g].bounds.error != FLT_MAX || result.groups[g].cluster_count != 1)
			continue;

		const MlcCluster root = result.clusters[result.groups[g].cluster_begin];
		current.assign(result.indices.begin() + root.index_begin, result.indices.begin() + root.index_begin + root.index_count);

		size_t group = g;
		float previous = root.refined >= 0 ? result.groups[size_t(root.refined)].bounds.error : 0.f;

		while (current.size() / 3 > tail_min_triangles)
		{
			size_t target = std::max(current.size() / 3 / 2, tail_min_triangles) * 3;

			float error = 0.f;
			clod::simplify(simplified, config, mesh, current, locks, target, &error);

			// an empty result must never become a zero-triangle cluster; stuck simplification ends the chain too
			if (simplified.empty() || simplified.size() > current.size() * config.simplify_threshold)
				break;

			float step = std::max(previous * config.simplify_error_merge_previous, error) + error * config.simplify_error_merge_additive;
			result.groups[group].bounds.error = step;

			MlcGroup next = {};
			next.depth = result.groups[group].depth + 1;
			next.bounds = result.groups[group].bounds;
			next.bounds.error = FLT_MAX;
			next.cluster_begin = result.clusters.size();
			next.cluster_count = 1;

			vertices.resize(simplified.size());
			triangles.resize(simplified.size());

			MlcCluster cluster = {};
			cluster.refined = int32_t(group);
			cluster.bounds = result.groups[group].bounds;
			cluster.index_begin = result.indices.size();
			cluster.index_count = simplified.size();
			cluster.vertex_count = clodLocalIndices(vertices.data(), triangles.data(), simplified.data(), simplified.size());
			result.indices.insert(result.indices.end(), simplified.begin(), simplified.end());
			result.clusters.push_back(cluster);
			result.groups.push_back(next);

			previous = step;
			group = result.groups.size() - 1;
			current.swap(simplified);
		}
	}
}

extern "C"
{

MlcResult* mlc_build(const MlcMesh* mesh, int preset, size_t max_triangles, size_t tail_min_triangles)
{
	if (!mesh || max_triangles < 4 || max_triangles > 256)
		return NULL;

	if (mesh->attribute_stride % sizeof(float) != 0)
		return NULL;

	try
	{
		clodConfig config = preset == MLC_PRESET_RT ? clodDefaultConfigRT(max_triangles) : clodDefaultConfig(max_triangles);

		clodMesh input = {};
		input.indices = mesh->indices;
		input.index_count = mesh->index_count;
		input.vertex_count = mesh->vertex_count;
		input.vertex_positions = mesh->positions;
		input.vertex_positions_stride = 3 * sizeof(float);
		input.vertex_attributes = mesh->attributes;
		input.vertex_attributes_stride = mesh->attribute_stride;
		input.attribute_weights = mesh->attribute_weights;
		input.attribute_count = mesh->attribute_count;
		input.attribute_protect_mask = mesh->attribute_protect_mask;
		input.vertex_lock = mesh->vertex_lock;

		std::unique_ptr<MlcResult> result(new MlcResult());
		clodBuild(config, input, result.get(), &collectGroup);
		if (tail_min_triangles > 0)
			appendTail(*result, config, input, tail_min_triangles);
		return result.release();
	}
	catch (...)
	{
		return NULL;
	}
}

void mlc_free(MlcResult* result)
{
	delete result;
}

size_t mlc_group_count(const MlcResult* result)
{
	return result->groups.size();
}

const MlcGroup* mlc_groups(const MlcResult* result)
{
	return result->groups.data();
}

size_t mlc_cluster_count(const MlcResult* result)
{
	return result->clusters.size();
}

const MlcCluster* mlc_clusters(const MlcResult* result)
{
	return result->clusters.data();
}

size_t mlc_index_count(const MlcResult* result)
{
	return result->indices.size();
}

const unsigned int* mlc_indices(const MlcResult* result)
{
	return result->indices.data();
}

size_t mlc_hierarchy_bound(size_t group_count, size_t node_width, size_t level_count)
{
	if (node_width < 2 || level_count == 0)
		return 0;

	return clodBuildHierarchyBound(group_count, node_width, level_count);
}

size_t mlc_build_hierarchy(MlcNode* nodes, size_t node_capacity, const MlcGroup* groups, size_t group_count, size_t node_width, size_t level_count)
{
	if (group_count == 0 || node_width < 2 || level_count == 0)
		return 0;

	// clodBuildHierarchy assumes every DAG level has at least one group; release builds skip its assert
	for (size_t level = 0; level < level_count; ++level)
	{
		bool found = false;
		for (size_t i = 0; i < group_count && !found; ++i)
			found = groups[i].depth == int32_t(level);

		if (!found)
			return 0;
	}

	size_t bound = clodBuildHierarchyBound(group_count, node_width, level_count);
	if (node_capacity < bound)
		return 0;

	try
	{
		std::vector<clodGroup> input(group_count);
		for (size_t i = 0; i < group_count; ++i)
		{
			input[i].depth = groups[i].depth;
			input[i].simplified = toClodBounds(groups[i].bounds);
		}

		std::vector<clodNode> output(bound);
		size_t used = clodBuildHierarchy(output.data(), input.data(), group_count, node_width, level_count);

		for (size_t i = 0; i < used; ++i)
		{
			MlcNode& node = nodes[i];
			node.bounds = toMlcBounds(output[i].bounds);
			node.group = output[i].group;
			node.child_offset = output[i].child_offset;
			node.child_count = output[i].child_count;
		}

		return used;
	}
	catch (...)
	{
		return 0;
	}
}

size_t mlc_local_indices(unsigned int* vertices, unsigned char* triangles, const unsigned int* indices, size_t index_count)
{
	if (index_count % 3 != 0 || index_count == 0)
		return 0;

	return clodLocalIndices(vertices, triangles, indices, index_count);
}

}
