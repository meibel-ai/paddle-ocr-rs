// glibc compatibility shim for Ubuntu 22.04 (glibc 2.35)
#include <stdlib.h>
#include <stdio.h>
#include <stdarg.h>

long __isoc23_strtol(const char *nptr, char **endptr, int base) {
    return strtol(nptr, endptr, base);
}
unsigned long __isoc23_strtoul(const char *nptr, char **endptr, int base) {
    return strtoul(nptr, endptr, base);
}
long long __isoc23_strtoll(const char *nptr, char **endptr, int base) {
    return strtoll(nptr, endptr, base);
}
unsigned long long __isoc23_strtoull(const char *nptr, char **endptr, int base) {
    return strtoull(nptr, endptr, base);
}
float __isoc23_strtof(const char *nptr, char **endptr) {
    return strtof(nptr, endptr);
}
double __isoc23_strtod(const char *nptr, char **endptr) {
    return strtod(nptr, endptr);
}
long double __isoc23_strtold(const char *nptr, char **endptr) {
    return strtold(nptr, endptr);
}
int __isoc23_fscanf(FILE *stream, const char *format, ...) {
    va_list args;
    va_start(args, format);
    int result = vfscanf(stream, format, args);
    va_end(args);
    return result;
}
int __isoc23_sscanf(const char *str, const char *format, ...) {
    va_list args;
    va_start(args, format);
    int result = vsscanf(str, format, args);
    va_end(args);
    return result;
}
