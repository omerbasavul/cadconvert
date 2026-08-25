/* cadconvert — Parasolid (.x_t), STEP (.stp) and glTF (.glb) to glTF binary
 * or USDZ.
 *
 * Ownership is one-directional: every string this library returns was
 * allocated by it and must come back to cadconvert_string_free. Nothing the
 * caller allocates is ever freed here. The one string that goes the other way
 * — the `detail` a progress callback receives — is lent for that call only.
 * No function unwinds; a panic inside the converter comes back as
 * CADCONVERT_ERR_PANIC.
 */
#ifndef CADCONVERT_H
#define CADCONVERT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* What to write. */
#define CADCONVERT_TARGET_PLAIN   0  /* positions and normals exactly as computed */
#define CADCONVERT_TARGET_LEAN    1  /* normals a byte a component; no vertex moves */
#define CADCONVERT_TARGET_COMPACT 2  /* positions on each mesh's own 16-bit grid too */
#define CADCONVERT_TARGET_USDZ    3  /* a USD package instead of glTF */

/* Outcomes. Zero is success; everything else leaves the summary untouched. */
#define CADCONVERT_OK                  0
#define CADCONVERT_ERR_NULL_ARGUMENT   1
#define CADCONVERT_ERR_BAD_UTF8        2
#define CADCONVERT_ERR_UNKNOWN_FORMAT  3
#define CADCONVERT_ERR_READ            4
#define CADCONVERT_ERR_WRITE           5
#define CADCONVERT_ERR_PANIC           6
#define CADCONVERT_ERR_CANCELLED       7  /* the progress callback said stop */

/* Where a conversion is, as the progress callback hears it. */
#define CADCONVERT_STAGE_READ  1  /* one unit: the input */
#define CADCONVERT_STAGE_MESH  2  /* a unit is a body; a file that arrived meshed has none */
#define CADCONVERT_STAGE_WRITE 3  /* a unit is an output file */

typedef struct {
    double  sag_mm;              /* mesh-to-surface distance in mm; 0 = the converter's own
                                    (0.04% of the model diagonal, so it scales) */
    double  angle_deg;           /* largest angle between facet normals; 0 = 8 degrees */
    int32_t target;              /* one of CADCONVERT_TARGET_* */
    int32_t use_parasolid_twin;  /* read a STEP file's .x_t twin for metal/matte */
} cadconvert_options;

typedef struct {
    uint64_t bytes;              /* every output added up */
    uint64_t bodies;
    uint64_t faces;              /* 0 for a file that arrived as a mesh */
    uint64_t faces_meshed;
    uint64_t triangles;
} cadconvert_summary;

/* Told between units of work, on the calling thread: `done` of `total` units
 * of `stage` are finished, and `detail` names the unit about to start (empty
 * when the stage is complete). Every stage reports at its start and after each
 * unit. Return 0 to continue, anything else to stop — the conversion then
 * returns CADCONVERT_ERR_CANCELLED. Must not unwind. */
typedef int32_t (*cadconvert_progress_fn)(void *user,
                                          int32_t stage,
                                          uint64_t done,
                                          uint64_t total,
                                          const char *detail);

/* Fill options with the defaults. A null pointer does nothing. */
void cadconvert_default_options(cadconvert_options *options);

/* The library's version. Static; never freed. */
const char *cadconvert_version(void);

/* Read input, mesh it, write output.
 *
 * On success returns CADCONVERT_OK, fills summary, and sets message to the
 * warnings — one per line — or to NULL when there were none. On failure
 * returns a CADCONVERT_ERR_* code and sets message to the reason. Either way a
 * non-NULL message must be given back to cadconvert_string_free.
 *
 * options, summary and message may each be NULL.
 */
int32_t cadconvert_convert(const char *input,
                           const char *output,
                           const cadconvert_options *options,
                           cadconvert_summary *summary,
                           char **message);

/* Read input once, mesh it once, and write it to each of output_count
 * outputs. Each output's extension chooses its container (.glb or .usdz), so a
 * part wanted in both is one call and one reading. progress, when not NULL,
 * is called between units of work with user passed back untouched. Everything
 * else is as cadconvert_convert; summary.bytes is every output added up.
 */
int32_t cadconvert_convert_many(const char *input,
                                const char *const *outputs,
                                size_t output_count,
                                const cadconvert_options *options,
                                cadconvert_progress_fn progress,
                                void *user,
                                cadconvert_summary *summary,
                                char **message);

/* Give back a string this library returned. NULL is accepted and ignored. */
void cadconvert_string_free(char *text);

#ifdef __cplusplus
}
#endif

#endif /* CADCONVERT_H */
