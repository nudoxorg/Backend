/* Ragel 6.10 control only: a four-step operation cursor over compact event bytes.
 * It deliberately exposes an untyped numeric `cs`; this is what the Rust candidate must justify. */
#include <stddef.h>
#include <stdint.h>

int cursor_run(const char *p, size_t length) {
    const char *pe = p + length;
    int cs = 0;
    uint8_t cursor_phase = 0;
    %%{
        machine operation_cursor;
        action requested { cursor_phase = 1; }
        action admitted  { cursor_phase = 2; }
        action staged    { cursor_phase = 3; }
        action verified  { cursor_phase = 4; }
        main := 'r' @requested 'a' @admitted 's' @staged 'v' @verified;
        write data;
        write init;
        write exec;
    }%%
    return cs >= operation_cursor_first_final && cursor_phase == 4 ? 1 : 0;
}
