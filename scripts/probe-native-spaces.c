/*
 * Read-only probe of the private SkyLight Space APIs.
 *
 * Companion to docs/reviews/window-manager-seam-deepening-2026-09-13.md, which
 * cites its output. It answers two questions the source could not:
 *
 *   1. What does SLSSpaceGetType report, for the Spaces in the managed list
 *      and for ids that name no Space? The binding's header documents only
 *      0 = user, 2 = system, 4 = fullscreen.
 *   2. Does SLSManagedDisplayGetCurrentSpace ever answer 0 for a display that
 *      has a current Space, i.e. is 0 a sentinel or a Space id?
 *
 * It performs no writes: no daemon, no window manipulation, no Space switch,
 * no focus change. Run it from a GUI (Aqua) session.
 *
 *   clang -O0 -Wall -framework CoreFoundation -framework CoreGraphics \
 *     -o /tmp/probe-native-spaces scripts/probe-native-spaces.c
 *   /tmp/probe-native-spaces
 *
 * Observed on macOS 26.6.2 (25G83), one display, three Spaces:
 *   - the managed list carried only types 0 and 4, and the dictionary's own
 *     `type` field agreed with SLSSpaceGetType on every entry;
 *   - every id naming no Space (0, 2, 3, 999, 999999, u64::MAX) returned 3,
 *     so 3 is an absent-Space sentinel that the header omits;
 *   - an unknown display UUID returned 0, so 0 means "no answer" and is not a
 *     Space id.
 */

#include <CoreFoundation/CoreFoundation.h>
#include <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

typedef int CGSConnectionID;
typedef uint64_t CGSSpaceID;

static void print_keys(CFDictionaryRef d) {
    CFIndex n = CFDictionaryGetCount(d);
    const void **keys = malloc(sizeof(void *) * (size_t)n);
    CFDictionaryGetKeysAndValues(d, keys, NULL);
    printf("      keys:");
    for (CFIndex i = 0; i < n; i++) {
        char b[128] = "?";
        if (CFGetTypeID(keys[i]) == CFStringGetTypeID())
            CFStringGetCString((CFStringRef)keys[i], b, sizeof b, kCFStringEncodingUTF8);
        printf(" %s", b);
    }
    printf("\n");
    free(keys);
}

int main(void) {
    void *h = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
    if (!h) { fprintf(stderr, "dlopen failed: %s\n", dlerror()); return 1; }

    CGSConnectionID (*pMainConn)(void) = dlsym(h, "SLSMainConnectionID");
    CFArrayRef (*pCopyDisplays)(CGSConnectionID) = dlsym(h, "SLSCopyManagedDisplaySpaces");
    int (*pSpaceType)(CGSConnectionID, CGSSpaceID) = dlsym(h, "SLSSpaceGetType");
    CGSSpaceID (*pCurrentSpace)(CGSConnectionID, CFStringRef) = dlsym(h, "SLSManagedDisplayGetCurrentSpace");
    CFUUIDRef (*pCreateUUID)(CGDirectDisplayID) = dlsym(h, "CGDisplayCreateUUIDFromDisplayID");
    if (!pMainConn || !pCopyDisplays || !pSpaceType || !pCurrentSpace || !pCreateUUID) {
        fprintf(stderr, "missing SkyLight symbols\n"); return 1;
    }

    CGSConnectionID cid = pMainConn();
    printf("SLSMainConnectionID = %d\n", cid);

    CFArrayRef displays = pCopyDisplays(cid);
    if (!displays) { fprintf(stderr, "SLSCopyManagedDisplaySpaces returned NULL\n"); return 1; }

    CFIndex n = CFArrayGetCount(displays);
    printf("managed display entries: %ld\n", (long)n);

    int type_histogram[16] = {0};
    for (CFIndex i = 0; i < n; i++) {
        CFDictionaryRef d = CFArrayGetValueAtIndex(displays, i);
        if (CFGetTypeID(d) != CFDictionaryGetTypeID()) { printf("  entry %ld is not a dict\n", (long)i); continue; }
        char ident[256] = "<none>";
        CFStringRef identRef = CFDictionaryGetValue(d, CFSTR("Display Identifier"));
        if (identRef && CFGetTypeID(identRef) == CFStringGetTypeID())
            CFStringGetCString(identRef, ident, sizeof ident, kCFStringEncodingUTF8);

        CFArrayRef spaces = CFDictionaryGetValue(d, CFSTR("Spaces"));
        printf("\ndisplay[%ld] identifier=%s spaces=%ld\n", (long)i, ident,
               spaces ? (long)CFArrayGetCount(spaces) : -1L);
        if (!spaces) continue;
        for (CFIndex j = 0; j < CFArrayGetCount(spaces); j++) {
            CFDictionaryRef s = CFArrayGetValueAtIndex(spaces, j);
            if (CFGetTypeID(s) != CFDictionaryGetTypeID()) continue;
            long long sid = -1;
            CFNumberRef idRef = CFDictionaryGetValue(s, CFSTR("id64"));
            if (idRef) CFNumberGetValue(idRef, kCFNumberLongLongType, &sid);
            int t = (sid >= 0) ? pSpaceType(cid, (CGSSpaceID)sid) : -999;
            if (t >= 0 && t < 16) type_histogram[t]++;
            long long dtype = -1;
            CFNumberRef typeRef = CFDictionaryGetValue(s, CFSTR("type"));
            if (typeRef) CFNumberGetValue(typeRef, kCFNumberLongLongType, &dtype);
            printf("    space id64=%lld  SLSSpaceGetType=%d  dict.type=%lld  agree=%s\n",
                   sid, t, dtype, (dtype == t) ? "yes" : "NO");
            if (j == 0) print_keys(s);
        }
    }

    printf("\nSLSSpaceGetType histogram (type->count):");
    for (int t = 0; t < 16; t++) if (type_histogram[t]) printf("  %d->%d", t, type_histogram[t]);
    printf("\n");

    printf("\nSLSSpaceGetType on ids that are not in the list above:\n");
    CGSSpaceID bogus[] = {0, 2, 3, 999, 999999, 18446744073709551615ULL};
    for (size_t i = 0; i < sizeof bogus / sizeof bogus[0]; i++)
        printf("  id=%-22llu -> %d\n", (unsigned long long)bogus[i], pSpaceType(cid, bogus[i]));

    CFStringRef bogusUUID = CFStringCreateWithCString(NULL, "00000000-0000-0000-0000-000000000000", kCFStringEncodingUTF8);
    printf("\nSLSManagedDisplayGetCurrentSpace with an unknown UUID -> %llu\n",
           (unsigned long long)pCurrentSpace(cid, bogusUUID));
    CFRelease(bogusUUID);

    uint32_t count = 0;
    CGGetActiveDisplayList(0, NULL, &count);
    uint32_t *ids = malloc(sizeof(uint32_t) * (size_t)count);
    CGGetActiveDisplayList(count, ids, &count);
    printf("\nactive displays: %u\n", count);
    for (uint32_t i = 0; i < count; i++) {
        CFUUIDRef uuid = pCreateUUID(ids[i]);
        CFStringRef uuidStr = uuid ? CFUUIDCreateString(NULL, uuid) : NULL;
        CGSSpaceID cur = uuidStr ? pCurrentSpace(cid, uuidStr) : 0;
        char ub[256] = "<none>";
        if (uuidStr) CFStringGetCString(uuidStr, ub, sizeof ub, kCFStringEncodingUTF8);
        printf("  display_id=%u uuid=%s SLSManagedDisplayGetCurrentSpace=%llu\n",
               ids[i], ub, (unsigned long long)cur);
        if (uuid) CFRelease(uuid);
        if (uuidStr) CFRelease(uuidStr);
    }
    free(ids);
    return 0;
}
