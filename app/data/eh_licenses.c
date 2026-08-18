/* eh_licenses.c — bundled third-party licenses (see eh_licenses.h).
 * The texts are the actual licenses of the components the app bundles
 * (cJSON is vendored in app/vendor; the EPUB/PDF/FB2 extraction, compiled
 * into the binary as the Rust staticlib, bundles zip/roxmltree/miniz_oxide
 * from rust_extract/) or links (SQLite via the store and progress DBs, zlib
 * via the firmware's image/network stack, libcurl via the PC/SDL HTTP
 * backend).  They live here as C strings so they ship inside the single
 * binary and are viewable on every platform — device, emulator and the PC
 * build — with no filesystem dependency.
 *
 * Text sources: cJSON from app/vendor/cJSON.c; SQLite blessing from
 * app/vendor/sqlite3.h; zlib from its LICENSE; libcurl from its
 * COPYING; zip/roxmltree/miniz_oxide from their crates' licenses. */

#include "eh_core.h"
#include "eh_licenses.h"

static const BsLicense g_licenses[] = {
    {
        "cJSON",
        "MIT",
        "JSON parser, bundled in app/vendor/cJSON.c",
        "Copyright (c) 2009-2017 Dave Gamble and cJSON contributors\n"
        "\n"
        "Permission is hereby granted, free of charge, to any person "
        "obtaining a copy of this software and associated documentation "
        "files (the \"Software\"), to deal in the Software without "
        "restriction, including without limitation the rights to use, "
        "copy, modify, merge, publish, distribute, sublicense, and/or "
        "sell copies of the Software, and to permit persons to whom the "
        "Software is furnished to do so, subject to the following "
        "conditions:\n"
        "\n"
        "The above copyright notice and this permission notice shall be "
        "included in all copies or substantial portions of the "
        "Software.\n"
        "\n"
        "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY "
        "KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE "
        "WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE "
        "AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT "
        "HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, "
        "WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING "
        "FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR "
        "OTHER DEALINGS IN THE SOFTWARE."
    },
    {
        "SQLite",
        "Public Domain",
        "Library store and reading-progress database (firmware/system SQLite)",
        "The author disclaims copyright to this source code.  In place "
        "of a legal notice, here is a blessing:\n"
        "\n"
        "    May you do good and not evil.\n"
        "    May you find forgiveness for yourself and forgive others.\n"
        "    May you share freely, never taking more than you give.\n"
        "\n"
        "SQLite is in the public domain and imposes no licence or "
        "attribution requirement."
    },
    {
        "zlib",
        "zlib",
        "firmware image/network stack (EPUB inflate is now the bundled Rust lib)",
        "Copyright notice:\n"
        "\n"
        " (C) 1995-2026 Jean-loup Gailly and Mark Adler\n"
        "\n"
        "  This software is provided 'as-is', without any express or "
        "implied warranty.  In no event will the authors be held liable "
        "for any damages arising from the use of this software.\n"
        "\n"
        "  Permission is granted to anyone to use this software for any "
        "purpose, including commercial applications, and to alter it "
        "and redistribute it freely, subject to the following "
        "restrictions:\n"
        "\n"
        "  1. The origin of this software must not be misrepresented; "
        "you must not claim that you wrote the original software.  If "
        "you use this software in a product, an acknowledgment in the "
        "product documentation would be appreciated but is not "
        "required.\n"
        "  2. Altered source versions must be plainly marked as such, "
        "and must not be misrepresented as being the original "
        "software.\n"
        "  3. This notice may not be removed or altered from any source "
        "distribution.\n"
        "\n"
        "  Jean-loup Gailly        Mark Adler\n"
        "  jloup@gzip.org          madler@alumni.caltech.edu"
    },
    {
        "Rust extraction (zip / roxmltree / miniz_oxide)",
        "MIT / Zlib",
        "EPUB/PDF/FB2 title, author and cover extraction (rust_extract staticlib)",
        "Handles: zip archive reading (zip crate, MIT), XML parsing"
        " (roxmltree, MIT or Apache-2.0), and raw-deflate inflation"
        " (miniz_oxide, MIT or Apache-2.0 or Zlib).\n"
        "\n"
        "MIT License: permission is hereby granted, free of charge, to"
        " any person obtaining a copy of this software and associated"
        " documentation files (the \u201cSoftware\u201d), to deal in the"
        " Software without restriction, including without limitation the"
        " rights to use, copy, modify, merge, publish, distribute,"
        " sublicense, and/or sell copies of the Software, and to permit"
        " persons to whom the Software is furnished to do so, subject to"
        " the following conditions:\n"
        "\n"
        "The above copyright notice and this permission notice shall be"
        " included in all copies or substantial portions of the"
        " Software.\n"
        "\n"
        "THE SOFTWARE IS PROVIDED \u201cAS IS\u201d, WITHOUT WARRANTY OF"
        " ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE"
        " WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE"
        " AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT"
        " HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,"
        " WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING"
        " FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR"
        " OTHER DEALINGS IN THE SOFTWARE."
    },
    {
        "libcurl",
        "MIT / ISC",
        "HTTP backend of the PC/SDL build (app/platform/eh_plat_sdl.c)",
        "COPYRIGHT AND PERMISSION NOTICE\n"
        "\n"
        "Copyright (c) 1996 - 2026, Daniel Stenberg, <daniel@haxx.se>, "
        "and many contributors, see the THANKS file.\n"
        "\n"
        "All rights reserved.\n"
        "\n"
        "Permission to use, copy, modify, and distribute this software "
        "for any purpose with or without fee is hereby granted, provided "
        "that the above copyright notice and this permission notice "
        "appear in all copies.\n"
        "\n"
        "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY "
        "KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE "
        "WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE "
        "AND NONINFRINGEMENT OF THIRD PARTY RIGHTS.  IN NO EVENT SHALL "
        "THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, "
        "DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, "
        "TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH "
        "THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n"
        "\n"
        "Except as contained in this notice, the name of a copyright "
        "holder shall not be used in advertising or otherwise to promote "
        "the sale, use or other dealings in this Software without prior "
        "written authorization of the copyright holder."
    },
};

int
eh_license_count(void)
{
    return (int)(sizeof g_licenses / sizeof g_licenses[0]);
}

const BsLicense *
eh_license(int i)
{
    if (i < 0 || i >= eh_license_count())
        return NULL;
    return &g_licenses[i];
}