/*
 * fmt.c - snprintf for the kernel and apps (integers and strings only).
 */
#include "string.h"

struct out {
    char *buf;
    size_t size, len;
};

static void out_c(struct out *o, char c) {
    if (o->len + 1 < o->size) o->buf[o->len] = c;
    o->len++;
}

static void out_num(struct out *o, uint64_t v, unsigned base, int width, char pad, int neg, int left) {
    char tmp[24];
    int n = 0;
    do {
        unsigned d = v % base;
        tmp[n++] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
        v /= base;
    } while (v);
    int len = n + neg;
    if (neg && pad == '0') out_c(o, '-'); /* the sign goes before zero padding: -0007 */
    if (!left)
        for (int i = len; i < width; i++) out_c(o, pad);
    if (neg && pad != '0') out_c(o, '-');
    while (n) out_c(o, tmp[--n]);
    if (left)
        for (int i = len; i < width; i++) out_c(o, ' ');
}

/* %s %c %d %u %x %p %%, with optional '-', '0', width and l/ll. */
int vsnprintf(char *buf, size_t size, const char *fmt, va_list ap) {
    struct out o = {buf, size, 0};
    for (; *fmt; fmt++) {
        if (*fmt != '%') {
            out_c(&o, *fmt);
            continue;
        }
        fmt++;
        int left = 0, width = 0, is_long = 0;
        char pad = ' ';
        if (*fmt == '-') left = 1, fmt++;
        if (*fmt == '0') pad = '0', fmt++;
        while (*fmt >= '0' && *fmt <= '9') width = width * 10 + (*fmt++ - '0');
        while (*fmt == 'l') is_long = 1, fmt++;
        switch (*fmt) {
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            int n = (int)strlen(s);
            if (!left)
                for (int i = n; i < width; i++) out_c(&o, ' ');
            while (*s) out_c(&o, *s++);
            if (left)
                for (int i = n; i < width; i++) out_c(&o, ' ');
            break;
        }
        case 'c': out_c(&o, (char)va_arg(ap, int)); break;
        case 'd': {
            int64_t v = is_long ? va_arg(ap, int64_t) : va_arg(ap, int);
            out_num(&o, v < 0 ? (uint64_t)-v : (uint64_t)v, 10, width, pad, v < 0, left);
            break;
        }
        case 'u': out_num(&o, is_long ? va_arg(ap, uint64_t) : va_arg(ap, unsigned), 10, width, pad, 0, left); break;
        case 'x': out_num(&o, is_long ? va_arg(ap, uint64_t) : va_arg(ap, unsigned), 16, width, pad, 0, left); break;
        case 'p':
            out_c(&o, '0');
            out_c(&o, 'x');
            out_num(&o, (uint64_t)va_arg(ap, void *), 16, 16, '0', 0, 0);
            break;
        case '%': out_c(&o, '%'); break;
        case 0: fmt--; break;
        default: out_c(&o, '%'); out_c(&o, *fmt); break;
        }
    }
    if (size) buf[o.len < size ? o.len : size - 1] = 0;
    return (int)o.len;
}

int snprintf(char *buf, size_t size, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, size, fmt, ap);
    va_end(ap);
    return n;
}
