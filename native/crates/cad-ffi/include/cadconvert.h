/* cadconvert — Parasolid (.x_t) and STEP (.stp) to glTF binary.
 *
 * Ownership is one-directional: every string this library returns was
 * allocated by it and must come back to cadconvert_string_free. Nothing the
 * caller allocates is ever freed here. No function unwinds; a panic inside the
 * converter comes back as CADCONVERT_ERR_PANIC.
 */
#ifndef CADCONVERT_H
#define CADCONVERT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* What to write. */
#define CADCONVERT_TARGET_PLAIN   0  /* positions and normals exactly as computed */
#define CADCONVERT_TARGET_LEAN    1  /* normals a byte a component; no vertex moves */
#define CADCONVERT_TARGET_COMPACT 2  /* positions on each mesh's own 16-bit grid too */

/* Outcomes. Zero is success; everything else leaves the summary untouched. */
#define CADCONVERT_OK                  0
#define CADCONVERT_ERR_NULL_ARGUMENT   1
#define CADCONVERT_ERR_BAD_UTF8        2
#define CADCONVERT_ERR_UNKNOWN_FORMAT  3
#define CADCONVERT_ERR_READ            4
#define CADCONVERT_ERR_WRITE           5
#define CADCONVERT_ERR_PANIC           6

typedef struct {
    double  sag_mm;              /* mesh-to-surface distance in mm; 0 = the converter's own
                                    (0.04% of the model diagonal, so it scales) */
    double  angle_deg;           /* largest angle between facet normals; 0 = 8 degrees */
    int32_t target;              /* one of CADCONVERT_TARGET_* */
    int32_t use_parasolid_twin;  /* read a STEP file's .x_t twin for metal/matte */
} cadconvert_options;

typedef struct {
    uint64_t bytes;
    uint64_t bodies;
    uint64_t faces;
    uint64_t faces_meshed;
    uint64_t triangles;
} cadconvert_summary;

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

/* Give back a string this library returned. NULL is accepted and ignored. */
void cadconvert_string_free(char *text);

#ifdef __cplusplus
}
#endif

#endif /* CADCONVERT_H */
