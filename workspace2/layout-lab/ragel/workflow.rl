/* Ragel 6.10 control only: a control-only workflow phase×event projection.
 * Payload/output equality and typed ReductionError facts are intentionally absent. */
#include <stddef.h>
#include <stdint.h>

int workflow_run(const char *p, size_t length) {
    const char *pe = p + length;
    int cs = 0;
    uint8_t workflow_phase = 0;
    %%{
        machine workflow_projection;
        action requested { workflow_phase = 1; }
        action admitted  { workflow_phase = 2; }
        action staged    { workflow_phase = 3; }
        action verified  { workflow_phase = 4; }
        action publishing { workflow_phase = 5; }
        action published { workflow_phase = 6; }
        main := 'r' @requested 'a' @admitted 's' @staged 'v' @verified 'p' @publishing 'u' @published;
        write data;
        write init;
        write exec;
    }%%
    return cs >= workflow_projection_first_final && workflow_phase == 6 ? 1 : 0;
}
