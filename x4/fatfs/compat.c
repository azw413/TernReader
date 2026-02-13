#include "ff.h"
#include <stddef.h>
#include <stdbool.h>

#if !defined(__STDC_VERSION__) || __STDC_VERSION__ < 201112L
#define static_assert(cond, msg) typedef char static_assertion_##__LINE__[(cond) ? 1 : -1]
#endif

bool ff_exists(const char* path) {
    FILINFO fno;
    FRESULT res = f_stat(path, &fno);
    return (res == FR_OK);
}

int ff_mount() {
    static FATFS fs;
    return f_mount(&fs, "", 1);
}

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
_Static_assert(sizeof(char) == 1, "char size mismatch");
_Static_assert(sizeof(BYTE) == 1, "BYTE size mismatch");
_Static_assert(sizeof(WORD) == 2, "WORD size mismatch");
_Static_assert(sizeof(DWORD) == 4, "DWORD size mismatch");
_Static_assert(sizeof(QWORD) == 8, "QWORD size mismatch");
_Static_assert(sizeof(WCHAR) == 2, "WCHAR size mismatch");
_Static_assert(sizeof(UINT) == 4, "UINT size mismatch");
_Static_assert(sizeof(FFOBJID) == 48, "FFOBJID size mismatch with Rust");
_Static_assert(sizeof(FIL) == 592, "FIL size mismatch with Rust");
_Static_assert(sizeof(DIR) == 80, "DIR size mismatch with Rust");
_Static_assert(sizeof(FILINFO) == 288, "FILINFO size mismatch with Rust");
#endif
