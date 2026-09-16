/*
 * Read-only probe of window identity within one login session.
 *
 * Companion to docs/wayfinding/declarative-state/issues/27-window-identity-verification.md
 * and docs/research/window-identity-2026-09-16.md, which cite its output. The
 * public documents state uniqueness but not lifetime, so it answers three
 * questions no documentation answers:
 *
 *   1. Is a kCGWindowNumber reused after its window is destroyed, inside one
 *      process and one login session? Immediately, or after a gap?
 *   2. Is NSWindow.windowNumber the same value as that window's
 *      kCGWindowNumber, and what does _AXUIElementGetWindow return for it?
 *   3. What does CFHash(AXUIElement) hash? Do two AX elements for the same
 *      window, reached through different paths, hash equal, and does the hash
 *      equal the element's pointer?
 *
 * It creates and destroys only its own small borderless windows. It performs no
 * writes to other applications, no daemon start, no Space or focus change. Run
 * it from a GUI (Aqua) session:
 *
 *   clang -fobjc-arc -O0 -Wall -framework AppKit -framework ApplicationServices \
 *     -o /tmp/probe-window-identity scripts/probe-window-identity.m
 *   /tmp/probe-window-identity [sequential_rounds] [prompt-trust]
 *
 * Question 3 has two halves: the object semantics of AXUIElement (pointer,
 * CFHash, CFEqual) need no permission and always run; the two-paths-to-one-
 * window half needs Accessibility permission for this process, and the probe
 * reports whether it is trusted and skips only that part when it is not.
 *
 * Observed on macOS 26.6.2 (25G83), one display, one login session: see the
 * research document for the results this produced.
 */

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>

extern AXError _AXUIElementGetWindow(AXUIElementRef element, CGWindowID *out);

static void pump(NSTimeInterval seconds) {
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:seconds]];
}

/// The window numbers the server currently lists for one process, both on- and
/// offscreen (`kCGWindowListOptionAll`).
static NSSet<NSNumber *> *cg_window_numbers(pid_t pid) {
    CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionAll, kCGNullWindowID);
    NSMutableSet *numbers = [NSMutableSet set];
    for (NSDictionary *info in (__bridge_transfer NSArray *)list) {
        if ([info[(id)kCGWindowOwnerPID] intValue] != pid) {
            continue;
        }
        [numbers addObject:info[(id)kCGWindowNumber]];
    }
    return numbers;
}

static NSWindow *make_window(int slot, BOOL identified) {
    NSRect frame = NSMakeRect(4, 4 + slot * 44, 40, 40);
    NSWindow *window = [[NSWindow alloc] initWithContentRect:frame
                                                   styleMask:NSWindowStyleMaskBorderless
                                                     backing:NSBackingStoreBuffered
                                                       defer:NO];
    if (identified) {
        window.identifier = [NSString stringWithFormat:@"spool.probe.%d", slot];
    }
    window.releasedWhenClosed = NO;
    [window orderFrontRegardless];
    return window;
}

/// The window number the server assigned to a window that did not exist before,
/// or -1 when the delta is not exactly one window.
static int created_window_number(pid_t pid, NSSet<NSNumber *> *before) {
    NSMutableSet *after = [cg_window_numbers(pid) mutableCopy];
    [after minusSet:before];
    return after.count == 1 ? [[after anyObject] intValue] : -1;
}

static void report_pair(const char *phase, int index, NSWindow *window, int cg_number) {
    printf("PAIR phase=%s index=%d ns_windowNumber=%ld cg_windowNumber=%d\n", phase, index,
           (long)window.windowNumber, cg_number);
}

/// Question 1, per-window: create, read, close, and watch whether the number
/// comes back to a later window.
static void probe_sequential_reuse(pid_t pid, int rounds) {
    NSMutableDictionary<NSNumber *, NSNumber *> *first_seen = [NSMutableDictionary dictionary];
    int reuses = 0;
    for (int i = 0; i < rounds; i++) {
        NSSet<NSNumber *> *before = cg_window_numbers(pid);
        NSWindow *window = make_window(i % 5, YES);
        pump(0.02);
        int cg = created_window_number(pid, before);
        if (i < 5 || cg < 0) {
            report_pair("sequential", i, window, cg);
        }
        NSNumber *key = @(cg);
        NSNumber *seen = first_seen[key];
        if (seen != nil) {
            reuses += 1;
            printf("REUSE_SEQUENTIAL index=%d cg_windowNumber=%d previous_index=%d gap=%d\n", i, cg,
                   seen.intValue, i - seen.intValue);
        } else {
            first_seen[key] = @(i);
        }
        [window orderOut:nil];
        [window close];
        pump(0.02);
    }
    printf("SEQUENTIAL_ROUNDS rounds=%d distinct_numbers=%lu reuses=%d\n", rounds,
           (unsigned long)first_seen.count, reuses);
}

/// Question 1, in batches: four windows at once, all closed, then four more.
static void probe_batch_reuse(pid_t pid, int batches) {
    for (int b = 0; b < batches; b++) {
        NSMutableSet<NSNumber *> *batch = [NSMutableSet set];
        NSMutableArray<NSWindow *> *windows = [NSMutableArray array];
        for (int i = 0; i < 4; i++) {
            NSSet<NSNumber *> *before = cg_window_numbers(pid);
            NSWindow *window = make_window(i, YES);
            [windows addObject:window];
            pump(0.02);
            NSNumber *number = @(created_window_number(pid, before));
            [batch addObject:number];
            if (b == 0) {
                report_pair("batch", i, window, number.intValue);
            }
        }
        for (NSWindow *window in windows) {
            [window orderOut:nil];
            [window close];
        }
        pump(0.05);
        printf("BATCH batch=%d numbers=%s\n", b,
               [[[batch.allObjects sortedArrayUsingSelector:@selector(compare:)]
                   valueForKey:@"description"] componentsJoinedByString:@","].UTF8String);
    }
}

/// Question 1, partial close: does the next window take the freed slot's
/// number, or the next one in sequence?
static void probe_partial_close(pid_t pid) {
    NSMutableArray<NSNumber *> *numbers = [NSMutableArray array];
    NSMutableArray<NSWindow *> *windows = [NSMutableArray array];
    for (int i = 0; i < 4; i++) {
        NSSet<NSNumber *> *before = cg_window_numbers(pid);
        NSWindow *window = make_window(i, YES);
        [windows addObject:window];
        pump(0.02);
        [numbers addObject:@(created_window_number(pid, before))];
    }
    printf("PARTIAL_BEFORE numbers=%s\n",
           [[numbers valueForKey:@"description"] componentsJoinedByString:@","].UTF8String);
    [windows[1] orderOut:nil];
    [windows[1] close];
    pump(0.05);
    NSSet<NSNumber *> *before = cg_window_numbers(pid);
    NSWindow *replacement = make_window(1, YES);
    pump(0.02);
    int cg = created_window_number(pid, before);
    printf("PARTIAL_AFTER freed=%d replacement=%d\n", numbers[1].intValue, cg);
    [replacement orderOut:nil];
    [replacement close];
    for (NSWindow *window in windows) {
        [window orderOut:nil];
        [window close];
    }
    pump(0.05);
}

/// Question 3's permission-free half: what AXUIElement's CoreFoundation
/// polymorphism actually compares. Two application elements for the same pid
/// are separate object claims about the same logical element.
static void probe_ax_object_semantics(pid_t pid, pid_t other_pid) {
    AXUIElementRef first = AXUIElementCreateApplication(pid);
    AXUIElementRef second = AXUIElementCreateApplication(pid);
    AXUIElementRef other = AXUIElementCreateApplication(other_pid);
    AXUIElementRef system = AXUIElementCreateSystemWide();
    printf("AX_OBJECT first=%llu second=%llu other=%llu system_wide=%llu\n",
           (unsigned long long)(uintptr_t)first, (unsigned long long)(uintptr_t)second,
           (unsigned long long)(uintptr_t)other, (unsigned long long)(uintptr_t)system);
    printf("AX_OBJECT_SAME_PID equal=%d same_hash=%d first_hash=%llu second_hash=%llu\n",
           CFEqual(first, second), CFHash(first) == CFHash(second),
           (unsigned long long)CFHash(first), (unsigned long long)CFHash(second));
    printf("AX_OBJECT_OTHER_PID equal=%d same_hash=%d\n", CFEqual(first, other),
           CFHash(first) == CFHash(other));
    printf("AX_OBJECT_HASH_IS_POINTER first=%d second=%d system=%d\n",
           CFHash(first) == (CFHashCode)(uintptr_t)first,
           CFHash(second) == (CFHashCode)(uintptr_t)second,
           CFHash(system) == (CFHashCode)(uintptr_t)system);
    // An untrusted client can still create elements; can it read one?
    CFTypeRef windows = NULL;
    AXError error = AXUIElementCopyAttributeValue(first, kAXWindowsAttribute, &windows);
    printf("AX_OBJECT_UNTRUSTED_READ error=%d value=%s\n", error,
           windows == NULL ? "(none)" : "(returned)");
    if (windows != NULL) {
        CFRelease(windows);
    }
    CFRelease(first);
    CFRelease(second);
    CFRelease(other);
    CFRelease(system);
}

static void print_element(const char *path, AXUIElementRef element) {
    CGWindowID window_id = 0;
    AXError window_error = _AXUIElementGetWindow(element, &window_id);
    CFTypeRef identifier = NULL;
    AXError identifier_error =
        AXUIElementCopyAttributeValue(element, kAXIdentifierAttribute, &identifier);
    CFTypeRef title = NULL;
    AXUIElementCopyAttributeValue(element, kAXTitleAttribute, &title);
    printf("AX_ELEMENT path=%s pointer=%llu hash=%llu window_id=%d window_error=%d "
           "identifier=%s identifier_error=%d title=%s\n",
           path, (unsigned long long)(uintptr_t)element,
           (unsigned long long)CFHash(element), window_id, window_error,
           identifier == NULL ? "(none)"
                              : [(__bridge NSString *)identifier UTF8String] ?: "(non-string)",
           identifier_error,
           title == NULL ? "(none)" : [(__bridge NSString *)title UTF8String] ?: "(non-string)");
    if (identifier != NULL) {
        CFRelease(identifier);
    }
    if (title != NULL) {
        CFRelease(title);
    }
}

/// Question 3: the same window through two AX paths, twice over.
static void probe_ax_identity(pid_t pid) {
    AXUIElementRef app = AXUIElementCreateApplication(pid);
    CFTypeRef window_list = NULL;
    AXError error = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &window_list);
    printf("AX_WINDOWS error=%d count=%ld\n", error,
           window_list == NULL ? -1L : (long)CFArrayGetCount(window_list));
    if (window_list == NULL) {
        CFRelease(app);
        return;
    }

    // Two separate fetches of the same attribute: does a repeat visit hash
    // equal the first?
    CFTypeRef second_list = NULL;
    AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &second_list);

    for (CFIndex i = 0; i < CFArrayGetCount(window_list); i++) {
        AXUIElementRef element = (AXUIElementRef)CFArrayGetValueAtIndex(window_list, i);
        char path[64];
        snprintf(path, sizeof(path), "windows[%ld]", (long)i);
        print_element(path, element);
        CGWindowID window_id = 0;
        if (_AXUIElementGetWindow(element, &window_id) != 0 || second_list == NULL) {
            continue;
        }
        for (CFIndex j = 0; j < CFArrayGetCount(second_list); j++) {
            AXUIElementRef other = (AXUIElementRef)CFArrayGetValueAtIndex(second_list, j);
            CGWindowID other_id = 0;
            if (_AXUIElementGetWindow(other, &other_id) != 0 || other_id != window_id) {
                continue;
            }
            printf("AX_REPEAT i=%ld j=%ld equal=%d same_hash=%d\n", (long)i, (long)j,
                   CFEqual(element, other), CFHash(element) == CFHash(other));
        }
    }

    // A different path to the same window: the focused one, and a hit test at
    // its centre.
    NSWindow *key = NSApp.keyWindow ?: NSApp.mainWindow;
    if (key != nil) {
        CFTypeRef focused = NULL;
        AXError focused_error =
            AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &focused);
        printf("AX_FOCUSED error=%d\n", focused_error);
        if (focused != NULL) {
            print_element("focused", (AXUIElementRef)focused);
            AXUIElementRef from_list = NULL;
            for (CFIndex i = 0; i < CFArrayGetCount(window_list); i++) {
                AXUIElementRef element = (AXUIElementRef)CFArrayGetValueAtIndex(window_list, i);
                CGWindowID a = 0, b = 0;
                if (_AXUIElementGetWindow(element, &a) == 0 && _AXUIElementGetWindow(focused, &b) == 0
                    && a == b) {
                    from_list = element;
                }
            }
            if (from_list != NULL) {
                printf("AX_PATH equal=%d same_hash=%d\n", CFEqual(from_list, focused),
                       CFHash(from_list) == CFHash(focused));
            }
            CFRelease(focused);
        }
        NSRect frame = key.frame;
        CGPoint point = CGPointMake(NSMidX(frame), NSMidY(frame));
        AXUIElementRef hit = NULL;
        AXError hit_error = AXUIElementCopyElementAtPosition(app, point.x, point.y, &hit);
        printf("AX_HIT error=%d point=%.0f,%.0f\n", hit_error, point.x, point.y);
        if (hit != NULL) {
            print_element("hit_test", hit);
            CFRelease(hit);
        }
    }

    CFRelease(app);
    CFRelease(window_list);
    if (second_list != NULL) {
        CFRelease(second_list);
    }
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        pid_t pid = getpid();
        pid_t parent = getppid();
        int rounds = argc > 1 ? atoi(argv[1]) : 40;
        if (argc > 2 && strcmp(argv[2], "prompt-trust") == 0) {
            const void *keys[] = {kAXTrustedCheckOptionPrompt};
            const void *values[] = {kCFBooleanTrue};
            CFDictionaryRef options = CFDictionaryCreate(NULL, keys, values, 1,
                                                         &kCFTypeDictionaryKeyCallBacks,
                                                         &kCFTypeDictionaryValueCallBacks);
            printf("AX_PROMPT trusted_before=%d\n", AXIsProcessTrustedWithOptions(options));
            CFRelease(options);
        }
        printf("PROBE pid=%d ppid=%d ax_trusted=%d rounds=%d\n", pid, parent,
               AXIsProcessTrusted() ? 1 : 0, rounds);
        pump(0.2);

        probe_ax_object_semantics(pid, parent);
        probe_sequential_reuse(pid, rounds);
        probe_batch_reuse(pid, 3);
        probe_partial_close(pid);

        // Question 2 needs a window that stays alive across the AX reads.
        NSWindow *target = make_window(0, YES);
        NSWindow *anonymous = make_window(1, NO);
        [target makeKeyAndOrderFront:nil];
        pump(0.2);
        printf("TARGET ns_windowNumber=%ld anonymous_ns_windowNumber=%ld\n",
               (long)target.windowNumber, (long)anonymous.windowNumber);
        if (AXIsProcessTrusted()) {
            probe_ax_identity(pid);
        } else {
            printf("AX_SKIPPED reason=not_trusted\n");
        }
        [target orderOut:nil];
        [target close];
        [anonymous orderOut:nil];
        [anonymous close];
        printf("DONE\n");
    }
    return 0;
}
