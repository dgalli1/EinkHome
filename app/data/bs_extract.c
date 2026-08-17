/* bs_extract.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_extract.h"

#include <zlib.h>

/* ── metadata extraction for local book files ──────────────────────────
 * The Local source imports files whose names carry no metadata; this
 * module pulls title / author / embedded cover out of the book files
 * themselves.  Only the formats worth the effort are parsed:
 *
 *   epub  — full ZIP read (META-INF/container.xml → OPF → dc:title /
 *           dc:creator / cover <meta>) with raw-deflate inflate.
 *   pdf   — literal /Title (/Author scan of the file tail.
 *   fb2   — XML <book-title> / <first-name> / <last-name>.
 *
 * Everything else falls back to the filename-derived title. */

/* ── small text helpers ──────────────────────────────────────────────── */

static void
trim_str(char *s)
{
    char *p = s;
    while (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')
        p++;
    if (p != s)
        memmove(s, p, strlen(p) + 1);
    size_t n = strlen(s);
    while (n > 0 && (s[n - 1] == ' ' || s[n - 1] == '\t' || s[n - 1] == '\r' || s[n - 1] == '\n'))
        s[--n] = '\0';
}

/* Decode a `&#NN;` numeric entity at `r`.  On success, writes its UTF-8
 * encoding via `*wp` and returns the pointer to continue after `;`;
 * otherwise returns NULL. */
static char *
xml_numeric_entity(char *r, char **wp)
{
    if (r[0] != '&')
        return NULL;
    if (r[1] != '#')
        return NULL;
    char *end = NULL;
    long  code = strtol(r + 2, &end, 10);
    if (end == NULL || *end != ';' || code <= 0 || code >= 0x110000)
        return NULL;
    /* Surrogates are not valid Unicode scalar values (the XML spec
     * forbids them); encode the replacement character instead of
     * emitting invalid UTF-8. */
    if (code >= 0xD800 && code <= 0xDFFF)
        code = 0xFFFD;
    char *w = *wp;
    if (code < 0x80) {
        *w++ = (char)code;
    } else if (code < 0x800) {
        *w++ = (char)(0xC0 | (code >> 6));
        *w++ = (char)(0x80 | (code & 0x3F));
    } else if (code < 0x10000) {
        *w++ = (char)(0xE0 | (code >> 12));
        *w++ = (char)(0x80 | ((code >> 6) & 0x3F));
        *w++ = (char)(0x80 | (code & 0x3F));
    } else {
        /* Up to 0x10FFFF: full 4-byte sequence. */
        *w++ = (char)(0xF0 | (code >> 18));
        *w++ = (char)(0x80 | ((code >> 12) & 0x3F));
        *w++ = (char)(0x80 | ((code >> 6) & 0x3F));
        *w++ = (char)(0x80 | (code & 0x3F));
    }
    *wp = w;
    return end + 1;
}

/* Basic XML entity unescape in place (named + common numeric). */
static void
xml_unescape(char *s)
{
    static const struct {
        const char *ent;
        char        ch;
    } ents[] = {{"&lt;", '<'}, {"&gt;", '>'}, {"&amp;", '&'}, {"&quot;", '"'}, {"&apos;", '\''}};
    char *w = s;
    for (char *r = s; *r;) {
        int done = 0;
        for (size_t i = 0; i < sizeof ents / sizeof ents[0]; i++) {
            size_t el = strlen(ents[i].ent);
            if (strncmp(r, ents[i].ent, el) == 0) {
                *w++ = ents[i].ch;
                r += el;
                done = 1;
                break;
            }
        }
        if (!done) {
            char *nr = xml_numeric_entity(r, &w);
            if (nr != NULL) {
                r = nr;
                done = 1;
            }
        }
        if (!done)
            *w++ = *r++;
    }
    *w = '\0';
}

/* Content of the first <tag>…</tag> (or <tag …>…</tag>); NULL if absent. */
static char *
xml_tag_content(const char *xml, const char *tag, char *out, size_t cap)
{
    char open[40], close[40];
    snprintf(open, sizeof open, "<%s", tag);
    snprintf(close, sizeof close, "</%s>", tag);
    const char *p = strstr(xml, open);
    if (p == NULL)
        return NULL;
    p = strchr(p, '>');
    if (p == NULL)
        return NULL;
    p++;
    const char *e = strstr(p, close);
    if (e == NULL)
        return NULL;
    size_t n = (size_t)(e - p);
    if (n >= cap)
        n = cap - 1;
    memcpy(out, p, n);
    out[n] = '\0';
    trim_str(out);
    xml_unescape(out);
    return out;
}

/* Value of attr="…" inside a tag fragment; -1 if absent. */
static int
xml_attr(const char *tag, const char *attr, char *out, size_t cap)
{
    char pat[40];
    snprintf(pat, sizeof pat, "%s=", attr);
    const char *p = strstr(tag, pat);
    if (p == NULL)
        return -1;
    p += strlen(pat);
    if (*p != '"' && *p != '\'')
        return -1;
    char        q = *p++;
    const char *e = strchr(p, q);
    if (e == NULL)
        return -1;
    size_t n = (size_t)(e - p);
    if (n >= cap)
        n = cap - 1;
    memcpy(out, p, n);
    out[n] = '\0';
    return 0;
}

/* ── ZIP reader (EPUB container) ─────────────────────────────────────── */

/* Scan the file tail for the end-of-central-directory record and read
 * the central-directory offset/size out of it.  Returns 0 on success. */
static int
zip_eocd(FILE *f, long fsize, unsigned long *cd_off, unsigned long *cd_size)
{
    /* EOCD: scan the last 64 KiB back for PK\x05\x06. */
    long           scan = fsize < 65536 ? fsize : 65536;
    unsigned char *tail = malloc((size_t)scan);
    if (tail == NULL)
        return -1;
    if (fseek(f, fsize - scan, SEEK_SET) != 0 || fread(tail, 1, (size_t)scan, f) != (size_t)scan) {
        free(tail);
        return -1;
    }
    long eocd = -1;
    for (long i = scan - 22; i >= 0; i--) {
        if (tail[i] == 'P' && tail[i + 1] == 'K' && tail[i + 2] == 5 && tail[i + 3] == 6) {
            eocd = i;
            break;
        }
    }
    if (eocd < 0) {
        free(tail);
        return -1;
    }
    *cd_off = 0;
    *cd_size = 0;
    for (int b = 0; b < 4; b++) {
        *cd_off |= (unsigned long)tail[eocd + 16 + b] << (8 * b);
        *cd_size |= (unsigned long)tail[eocd + 12 + b] << (8 * b);
    }
    free(tail);
    return 0;
}

/* Walk the central directory for the entry named `want`.  On a match,
 * fills in `method`/`comp_size`/`uncomp`/`local_off` and returns 0;
 * returns -1 if not found. */
static int
zip_find_entry(const unsigned char *cd, size_t cd_size, const char *want,
               unsigned long *method, unsigned long *comp_size,
               unsigned long *uncomp, unsigned long *local_off)
{
    size_t pos = 0;
    while (pos + 46 <= cd_size) {
        if (cd[pos] != 'P' || cd[pos + 1] != 'K' || cd[pos + 2] != 1 || cd[pos + 3] != 2)
            break;
        unsigned int name_len = cd[pos + 28] | (unsigned int)cd[pos + 29] << 8;
        unsigned int extra_len = cd[pos + 30] | (unsigned int)cd[pos + 31] << 8;
        unsigned int cmt_len = cd[pos + 32] | (unsigned int)cd[pos + 33] << 8;
        if (pos + 46 + name_len > cd_size)
            break;
        char   name[512];
        size_t nl = name_len < sizeof name - 1 ? name_len : sizeof name - 1;
        memcpy(name, cd + pos + 46, nl);
        name[nl] = '\0';
        if (strcmp(name, want) == 0) {
            /* method is a 2-byte field (offsets 10-11); the rest are
             * 4-byte. */
            *method = (unsigned long)(cd[pos + 10] | (unsigned int)cd[pos + 11] << 8);
            *comp_size = 0;
            *uncomp = 0;
            *local_off = 0;
            for (int b = 0; b < 4; b++) {
                *comp_size |= (unsigned long)cd[pos + 20 + b] << (8 * b);
                *uncomp |= (unsigned long)cd[pos + 24 + b] << (8 * b);
                *local_off |= (unsigned long)cd[pos + 42 + b] << (8 * b);
            }
            return 0;
        }
        pos += 46 + name_len + extra_len + cmt_len;
    }
    return -1;
}

/* Read the compressed data of the entry whose local header sits at
 * `local_off`.  Returns 0 with `*comp_out` set (caller frees). */
static int
zip_read_local(FILE *f, unsigned long local_off, unsigned long comp_size,
               unsigned char **comp_out)
{
    /* Local header → data. */
    unsigned char lh[30];
    if (fseek(f, (long)local_off, SEEK_SET) != 0 || fread(lh, 1, sizeof lh, f) != sizeof lh)
        return -1;
    if (lh[0] != 'P' || lh[1] != 'K' || lh[2] != 3 || lh[3] != 4)
        return -1;
    unsigned int lname = lh[26] | (unsigned int)lh[27] << 8;
    unsigned int lextra = lh[28] | (unsigned int)lh[29] << 8;
    if (fseek(f, (long)local_off + 30 + lname + lextra, SEEK_SET) != 0)
        return -1;
    unsigned char *comp = malloc(comp_size ? comp_size : 1);
    if (comp == NULL)
        return -1;
    if (fread(comp, 1, comp_size, f) != comp_size) {
        free(comp);
        return -1;
    }
    *comp_out = comp;
    return 0;
}

/* Inflate a raw-deflate stream into a malloc'd buffer.  Returns 0 with
 * `*out`/`*out_len` set (caller frees), -1 on failure. */
static int
zip_inflate(const unsigned char *comp, unsigned long comp_size,
            unsigned long uncomp, unsigned char **out, size_t *out_len)
{
    size_t         cap = uncomp ? (size_t)uncomp + 1024 : 65536;
    unsigned char *buf = malloc(cap + 1);
    if (buf == NULL)
        return -1;
    z_stream strm;
    memset(&strm, 0, sizeof strm);
    strm.next_in = (unsigned char *)comp;
    strm.avail_in = (unsigned int)comp_size;
    strm.next_out = buf;
    strm.avail_out = (unsigned int)cap;
    if (inflateInit2(&strm, -MAX_WBITS) != Z_OK) {
        free(buf);
        return -1;
    }
    int rc;
    for (;;) {
        rc = inflate(&strm, Z_FINISH);
        if (rc == Z_STREAM_END)
            break;
        if (rc != Z_OK || strm.avail_out > 0)
            break;
        /* output full: grow and continue */
        size_t         newcap = cap * 2;
        unsigned char *nb = realloc(buf, newcap + 1);
        if (nb == NULL) {
            rc = Z_MEM_ERROR;
            break;
        }
        buf = nb;
        strm.next_out = buf + cap;
        strm.avail_out = (unsigned int)(newcap - cap);
        cap = newcap;
    }
    inflateEnd(&strm);
    if (rc != Z_STREAM_END) {
        free(buf);
        return -1;
    }
    *out = buf;
    *out_len = (size_t)strm.total_out;
    return 0;
}

/* Inflate one entry of a ZIP archive into a malloc'd buffer.  Returns 0
 * on success with *out_len set (caller frees), -1 otherwise. */
static int
zip_entry_read(const char *zip_path, const char *want, unsigned char **out, size_t *out_len)
{
    FILE *f = fopen(zip_path, "rb");
    if (f == NULL)
        return -1;
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return -1;
    }
    long fsize = ftell(f);
    if (fsize < 22) {
        fclose(f);
        return -1;
    }

    unsigned long cd_off = 0, cd_size = 0;
    if (zip_eocd(f, fsize, &cd_off, &cd_size) != 0) {
        fclose(f);
        return -1;
    }

    unsigned char *cd = malloc(cd_size ? cd_size : 1);
    if (cd == NULL) {
        fclose(f);
        return -1;
    }
    if (fseek(f, (long)cd_off, SEEK_SET) != 0 || fread(cd, 1, cd_size, f) != cd_size) {
        free(cd);
        fclose(f);
        return -1;
    }

    unsigned long method = 0, comp_size = 0, uncomp = 0, local_off = 0;
    if (zip_find_entry(cd, cd_size, want, &method, &comp_size, &uncomp, &local_off) != 0) {
        free(cd);
        fclose(f);
        return -1;
    }
    free(cd);

    unsigned char *comp = NULL;
    if (zip_read_local(f, local_off, comp_size, &comp) != 0) {
        fclose(f);
        return -1;
    }
    fclose(f);

    if (method == 0) { /* stored */
        *out = comp;
        *out_len = (size_t)comp_size;
        return 0;
    }
    if (method != 8) { /* deflate only */
        free(comp);
        return -1;
    }

    int rc = zip_inflate(comp, comp_size, uncomp, out, out_len);
    free(comp);
    return rc;
}

/* ── EPUB ────────────────────────────────────────────────────────────── */

/* Resolve an OPF href against the OPF's directory into an entry name. */
static void
epub_entry_path(const char *opf, const char *href, char *out, size_t cap)
{
    const char *slash = strrchr(opf, '/');
    if (slash != NULL)
        snprintf(out, cap, "%.*s/%s", (int)(slash - opf), opf, href);
    else
        snprintf(out, cap, "%s", href);
    if (out[0] == '/')
        memmove(out, out + 1, strlen(out));
}

/* Read META-INF/container.xml and return the OPF full-path entry name.
 * Returns 0 with `opf` set on success, -1 otherwise. */
static int
epub_opf_path(const char *path, char *opf, size_t cap)
{
    unsigned char *xml = NULL;
    size_t         xlen = 0;
    if (zip_entry_read(path, "META-INF/container.xml", &xml, &xlen) != 0)
        return -1;
    xml_attr((const char *)xml, "full-path", opf, cap);
    free(xml);
    return opf[0] == '\0' ? -1 : 0;
}

/* Find the `content` id of the `<meta name="cover">` tag.  Returns 0
 * with `cid` set if found, -1 otherwise. */
static int
epub_cover_id(const char *xml, char *cid, size_t cap)
{
    const char *p = xml;
    while ((p = strstr(p, "<meta")) != NULL) {
        const char *gt = strchr(p, '>');
        if (gt == NULL)
            break;
        char   tag[256];
        size_t tn = (size_t)(gt - p);
        if (tn >= sizeof tag)
            tn = sizeof tag - 1;
        memcpy(tag, p, tn);
        tag[tn] = '\0';
        char nv[64] = "";
        if (xml_attr(tag, "name", nv, sizeof nv) == 0 && strcmp(nv, "cover") == 0) {
            if (xml_attr(tag, "content", cid, cap) != 0)
                return -1;
            return 0;
        }
        p = gt + 1;
    }
    return -1;
}

/* Write the `<item id="cid">` image (if any, and if it is JPEG/PNG) out
 * to `cover_out`. */
static void
epub_write_image_file(const char *cover_out, const unsigned char *img, size_t ilen)
{
    int is_jpg = ilen > 2 && img[0] == 0xff && img[1] == 0xd8;
    int is_png = ilen > 4 && img[0] == 0x89 && img[1] == 'P';
    if (is_jpg || is_png) {
        FILE *o = fopen(cover_out, "wb");
        if (o != NULL) {
            fwrite(img, 1, ilen, o);
            fclose(o);
        }
    }
}

static void
epub_cover_write(const char *zip, const char *xml, const char *opf,
                 const char *cid, char *cover_out)
{
    char        want[200];
    snprintf(want, sizeof want, "id=\"%s\"", cid);
    const char *it = strstr(xml, "<item");
    while (it != NULL) {
        const char *gt = strchr(it, '>');
        if (gt == NULL)
            break;
        char   tag[320];
        size_t tn = (size_t)(gt - it);
        if (tn >= sizeof tag)
            tn = sizeof tag - 1;
        memcpy(tag, it, tn);
        tag[tn] = '\0';
        if (strstr(tag, want) != NULL) {
            char href[300] = "";
            if (xml_attr(tag, "href", href, sizeof href) == 0) {
                char          entry[320];
                epub_entry_path(opf, href, entry, sizeof entry);
                unsigned char *img = NULL;
                size_t         ilen = 0;
                if (zip_entry_read(zip, entry, &img, &ilen) == 0) {
                    epub_write_image_file(cover_out, img, ilen);
                    free(img);
                }
            }
            break;
        }
        it = gt + 1;
    }
}

/* Write the embedded cover of an OPF document out to `cover_out`. */
static void
epub_cover(const char *zip, const char *xml, const char *opf, char *cover_out)
{
    char cid[160] = "";
    if (epub_cover_id(xml, cid, sizeof cid) != 0)
        return;
    epub_cover_write(zip, xml, opf, cid, cover_out);
}

static int
epub_meta(const char *path,
          char       *title,
          size_t      title_cap,
          char       *author,
          size_t      author_cap,
          char       *cover_out,
          size_t      cover_cap)
{
    (void)cover_cap; /* out_path is a fixed temp path; cap unused */
    char opf[512] = "";
    if (epub_opf_path(path, opf, sizeof opf) != 0)
        return -1;
    if (opf[0] == '/')
        memmove(opf, opf + 1, strlen(opf));

    unsigned char *xml = NULL;
    size_t         xlen = 0;
    if (zip_entry_read(path, opf, &xml, &xlen) != 0)
        return -1;

    if (title != NULL)
        xml_tag_content((const char *)xml, "dc:title", title, title_cap);
    if (author != NULL)
        xml_tag_content((const char *)xml, "dc:creator", author, author_cap);

    if (cover_out != NULL)
        epub_cover(path, (const char *)xml, opf, cover_out);

    free(xml);
    return 0;
}

/* ── PDF ─────────────────────────────────────────────────────────────── */

/* Literal `/Key (value)` scan (uncompressed Info dicts). */
static void
pdf_find_string(const char *buf, const char *key, char *out, size_t cap)
{
    out[0] = '\0';
    const char *p = strstr(buf, key);
    while (p != NULL) {
        const char *q = p + strlen(key);
        while (*q == ' ' || *q == '\t' || *q == '\r' || *q == '\n')
            q++;
        if (*q == '(') {
            q++;
            size_t n = 0;
            int    esc = 0;
            while (*q != '\0' && n + 1 < cap) {
                if (esc) {
                    out[n++] = *q == 'n' ? '\n' : *q;
                    esc = 0;
                    q++;
                    continue;
                }
                if (*q == '\\') {
                    esc = 1;
                    q++;
                    continue;
                }
                if (*q == ')')
                    break;
                out[n++] = *q;
                q++;
            }
            out[n] = '\0';
            trim_str(out);
            return;
        }
        p = strstr(q, key);
    }
}

static int
pdf_meta(const char *path, char *title, size_t title_cap, char *author, size_t author_cap)
{
    FILE *f = fopen(path, "rb");
    if (f == NULL)
        return -1;
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return -1;
    }
    long sz = ftell(f);
    long want = sz > 262144 ? 262144 : sz;
    if (want <= 0) {
        fclose(f);
        return -1;
    }
    unsigned char *buf = malloc((size_t)want + 1);
    if (buf == NULL) {
        fclose(f);
        return -1;
    }
    if (fseek(f, sz - want, SEEK_SET) != 0 || fread(buf, 1, (size_t)want, f) != (size_t)want) {
        free(buf);
        fclose(f);
        return -1;
    }
    fclose(f);
    buf[want] = '\0';
    if (title != NULL)
        pdf_find_string((const char *)buf, "/Title", title, title_cap);
    if (author != NULL)
        pdf_find_string((const char *)buf, "/Author", author, author_cap);
    free(buf);
    return 0;
}

/* ── FB2 ─────────────────────────────────────────────────────────────── */

static int
fb2_meta(const char *path, char *title, size_t title_cap, char *author, size_t author_cap)
{
    FILE *f = fopen(path, "rb");
    if (f == NULL)
        return -1;
    /* 256KB on the task stack overflows the device stack (a 32KB frame
     * already crashed a boot loop on hardware); heap it like pdf_meta
     * does. */
    unsigned char *buf = malloc(262144);
    if (buf == NULL) {
        fclose(f);
        return -1;
    }
    size_t got = fread(buf, 1, 262144 - 1, f);
    fclose(f);
    if (got == 0) {
        free(buf);
        return -1;
    }
    buf[got] = '\0';
    if (title != NULL)
        xml_tag_content((const char *)buf, "book-title", title, title_cap);
    if (author != NULL && author_cap > 0) {
        char fn[80], ln[80];
        fn[0] = ln[0] = '\0';
        xml_tag_content((const char *)buf, "first-name", fn, sizeof fn);
        xml_tag_content((const char *)buf, "last-name", ln, sizeof ln);
        if (fn[0] != '\0' || ln[0] != '\0')
            snprintf(author, author_cap, "%s%s%s", fn, ln[0] ? " " : "", ln);
    }
    free(buf);
    return 0;
}

/* ── public API ──────────────────────────────────────────────────────── */

/* Extract title/author from a local book file.  Returns 0 when the
 * format was parsed (fields may still be empty → caller falls back to
 * the filename), -1 for unsupported formats. */
int
bs_extract_book_meta(const char *path,
                  const char *ext,
                  char       *title,
                  size_t      title_cap,
                  char       *author,
                  size_t      author_cap)
{
    if (title != NULL && title_cap > 0)
        title[0] = '\0';
    if (author != NULL && author_cap > 0)
        author[0] = '\0';
    if (ext == NULL)
        return -1;
    if (strcmp(ext, "epub") == 0)
        return epub_meta(path, title, title_cap, author, author_cap, NULL, 0);
    if (strcmp(ext, "pdf") == 0)
        return pdf_meta(path, title, title_cap, author, author_cap);
    if (strcmp(ext, "fb2") == 0)
        return fb2_meta(path, title, title_cap, author, author_cap);
    return -1;
}

/* Extract the embedded cover of a local book into `out_path` (a temp
 * file the cover pipeline can load).  `out_path` must already hold a
 * writable path — it is NOT cleared.  Returns 0 with the file written,
 * -1 when the book has no readable cover. */
int
bs_extract_book_cover(const char *path, const char *ext, char *out_path, size_t out_cap)
{
    if (ext == NULL)
        return -1;
    if (strcmp(ext, "epub") == 0)
        return epub_meta(path, NULL, 0, NULL, 0, out_path, out_cap);
    return -1;
}
