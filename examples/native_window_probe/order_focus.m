// Capability probe: does SLSOrderWindow reorder a FOREIGN window, and does any
// variant of it (or AXRaise) move frontmost-app / keyboard focus?
//
// Every mutation is either a pure no-op or is restored before exit, and the
// probe never calls order=0 (which means "order out" in CGS, i.e. it would hide
// someone else's window). Frontmost app is restored with
// -[NSRunningApplication activateWithOptions:] if any experiment changed it.
//
// Build: clang -fobjc-arc -framework Cocoa -framework CoreGraphics -o order_focus order_focus.m
#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ApplicationServices/ApplicationServices.h>
#include <dlfcn.h>

extern AXError _AXUIElementGetWindow(AXUIElementRef element, CGWindowID *window_id);

static int (*SLSOrderWindow)(int, uint32_t, int, uint32_t);
static int (*SLSMainConnectionID)(void);

static NSString *front_app(void) {
    NSRunningApplication *app = NSWorkspace.sharedWorkspace.frontmostApplication;
    return [NSString stringWithFormat:@"%@(%d)", app.bundleIdentifier ?: @"?", app.processIdentifier];
}

// Front-to-back order of layer-0 on-screen windows, abbreviated.
static NSArray<NSNumber *> *session_order(NSUInteger limit) {
    NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    NSMutableArray<NSNumber *> *ids = [NSMutableArray array];
    for (NSDictionary *window in windows) {
        if ([window[(id)kCGWindowLayer] intValue] != 0) continue;
        [ids addObject:window[(id)kCGWindowNumber]];
        if (ids.count >= limit) break;
    }
    return ids;
}

static NSString *state(void) {
    NSArray<NSNumber *> *order = session_order(5);
    NSMutableArray *text = [NSMutableArray array];
    for (NSNumber *window_id in order) [text addObject:window_id.stringValue];
    return [NSString stringWithFormat:@"front=%@ top5=[%@]", front_app(), [text componentsJoinedByString:@","]];
}

// Index of a window in the front-to-back list, or -1 when it is not on screen.
static NSInteger rank(CGWindowID window_id) {
    NSUInteger index = [session_order(500) indexOfObject:@(window_id)];
    return index == NSNotFound ? -1 : (NSInteger)index;
}

// A window only enters the WindowServer on-screen list after AppKit has been
// launched and the run loop has turned at least once, even with
// -orderFrontRegardless. Without this the probe cannot observe its own windows.
static void pump_until_visible(NSArray<NSNumber *> *window_ids) {
    for (int attempt = 0; attempt < 30; attempt++) {
        BOOL waiting = NO;
        for (NSNumber *window_id in window_ids)
            if (rank(window_id.unsignedIntValue) < 0) waiting = YES;
        if (!waiting) return;
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.1]];
    }
    NSLog(@"warning: probe windows never entered the on-screen window list");
}

static CGWindowID finder_window(NSArray<NSNumber *> *skip, BOOL want_front) {
    NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    NSMutableArray<NSNumber *> *found = [NSMutableArray array];
    for (NSDictionary *window in windows) {
        if ([window[(id)kCGWindowLayer] intValue] != 0) continue;
        pid_t pid = [window[(id)kCGWindowOwnerPID] intValue];
        NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
        if (![app.bundleIdentifier isEqual:@"com.apple.finder"]) continue;
        NSNumber *window_id = window[(id)kCGWindowNumber];
        if ([skip containsObject:window_id]) continue;
        [found addObject:window_id];
    }
    if (found.count == 0) return 0;
    return (want_front ? found.firstObject : found.lastObject).unsignedIntValue;
}

// Attempts `order(target, placement, relative_to)` and reports what changed.
static void attempt(NSString *label, CGWindowID target, int placement, CGWindowID relative_to) {
    NSInteger before_target = rank(target);
    NSInteger before_relative = rank(relative_to);
    NSString *before_front = front_app();
    int result = SLSOrderWindow(SLSMainConnectionID(), target, placement, relative_to);
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.25]];
    NSInteger after_target = rank(target);
    NSInteger after_relative = rank(relative_to);
    NSLog(@"%@: target=%u rel=%u place=%d result=%d\n    rank target %ld->%ld rel %ld->%ld\n    before %@\n    after  %@%@",
          label, target, relative_to, placement, result,
          (long)before_target, (long)after_target, (long)before_relative, (long)after_relative,
          before_front, state(),
          [before_front isEqualToString:front_app()] ? @"" : @"   <-- FRONT APP CHANGED");
}

// The owning application's AX focused/main window id, so a raise can be checked
// for per-app focus side effects, not just global frontmost changes.
static CGWindowID app_ax_window(pid_t pid, CFStringRef attribute) {
    AXUIElementRef application = AXUIElementCreateApplication(pid);
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(application, attribute, &value) != kAXErrorSuccess || !value) {
        CFRelease(application);
        return 0;
    }
    CGWindowID window_id = 0;
    _AXUIElementGetWindow((AXUIElementRef)value, &window_id);
    CFRelease(value);
    CFRelease(application);
    return window_id;
}

// AXRaise on a foreign window, for comparison: does it steal focus?
static void attempt_ax_raise(CGWindowID window_id, pid_t pid) {
    if (!AXIsProcessTrusted()) {
        NSLog(@"ax_raise: skipped (probe process is not accessibility-trusted)");
        return;
    }
    AXUIElementRef application = AXUIElementCreateApplication(pid);
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(application, kAXWindowsAttribute, &value) != kAXErrorSuccess || !value) {
        NSLog(@"ax_raise: no AXWindows for pid %d", pid);
        CFRelease(application);
        return;
    }
    NSString *before_front = front_app();
    NSInteger before = rank(window_id);
    CGWindowID before_focused = app_ax_window(pid, kAXFocusedWindowAttribute);
    CGWindowID before_main = app_ax_window(pid, kAXMainWindowAttribute);
    for (id window in (__bridge NSArray *)value) {
        CGWindowID candidate = 0;
        _AXUIElementGetWindow((__bridge AXUIElementRef)window, &candidate);
        if (candidate != window_id) continue;
        AXUIElementPerformAction((__bridge AXUIElementRef)window, kAXRaiseAction);
        break;
    }
    CFRelease(value);
    CFRelease(application);
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.3]];
    NSLog(@"ax_raise: target=%u rank %ld->%ld app focused %u->%u main %u->%u\n    before %@\n    after  %@%@",
          window_id, (long)before, (long)rank(window_id),
          before_focused, app_ax_window(pid, kAXFocusedWindowAttribute),
          before_main, app_ax_window(pid, kAXMainWindowAttribute),
          before_front, state(),
          [before_front isEqualToString:front_app()] ? @"" : @"   <-- FRONT APP CHANGED");
}

int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        // Without a bundle an AppKit process defaults to a prohibited activation
        // policy, and its windows never enter the WindowServer on-screen list.
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [NSApp finishLaunching];
        void *library = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
        SLSMainConnectionID = dlsym(library, "SLSMainConnectionID");
        SLSOrderWindow = dlsym(library, "SLSOrderWindow");
        if (!SLSMainConnectionID || !SLSOrderWindow) {
            NSLog(@"missing SkyLight symbols");
            return 2;
        }

        NSRunningApplication *original_front = NSWorkspace.sharedWorkspace.frontmostApplication;
        NSRect screen = NSScreen.mainScreen.visibleFrame;
        NSWindow *own_a = [[NSWindow alloc]
            initWithContentRect:NSMakeRect(NSMaxX(screen) - 90, NSMinY(screen) + 10, 40, 40)
                      styleMask:NSWindowStyleMaskBorderless
                        backing:NSBackingStoreBuffered
                          defer:NO];
        NSWindow *own_b = [[NSWindow alloc]
            initWithContentRect:NSMakeRect(NSMaxX(screen) - 90, NSMinY(screen) + 60, 40, 40)
                      styleMask:NSWindowStyleMaskBorderless
                        backing:NSBackingStoreBuffered
                          defer:NO];
        [own_a orderFrontRegardless];
        [own_b orderFrontRegardless];
        CGWindowID probe_a = (CGWindowID)own_a.windowNumber;
        CGWindowID probe_b = (CGWindowID)own_b.windowNumber;
        pump_until_visible(@[ @(probe_a), @(probe_b) ]);

        CGWindowID finder_front = finder_window(@[], YES);
        CGWindowID finder_back = finder_window(@[ @(finder_front) ], NO);
        NSLog(@"probe_windows=%u,%u finder_front=%u finder_back=%u ax_trusted=%d\n    %@",
              probe_a, probe_b, finder_front, finder_back, AXIsProcessTrusted(), state());

        // Control: the public AppKit relative order must show up in the same
        // front-to-back list, so a no-op from SLSOrderWindow is a property of the
        // private call and not of the probe's own windows.
        [own_b orderWindow:NSWindowBelow relativeTo:own_a.windowNumber];
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.3]];
        NSLog(@"control [AppKit: A above B]: A rank=%ld B rank=%ld", (long)rank(probe_a), (long)rank(probe_b));

        // Each call below is a real change relative to the control state.
        attempt(@"own: B above A", probe_b, 1, probe_a);
        attempt(@"own: A above B", probe_a, 1, probe_b);
        attempt(@"own: A to front (no relative)", probe_a, 1, 0);

        if (finder_front) {
            attempt(@"own above foreign", probe_a, 1, finder_front);
            attempt(@"foreign above own", finder_front, 1, probe_a);
            attempt(@"foreign below own", finder_front, -1, probe_a);
        }

        // `order` 0 (out) and 2 (in) do act on own windows, which proves the call
        // reaches WindowServer instead of being discarded; 3 is rejected. This is
        // the evidence that "returns 0" is not the same as "relative order applied".
        NSInteger before_out = rank(probe_a);
        int out_result = SLSOrderWindow(SLSMainConnectionID(), probe_a, 0, 0);
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.3]];
        NSInteger after_out = rank(probe_a);
        int in_result = SLSOrderWindow(SLSMainConnectionID(), probe_a, 2, 0);
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.3]];
        NSInteger after_in = rank(probe_a);
        int invalid_result = SLSOrderWindow(SLSMainConnectionID(), probe_a, 3, 0);
        NSLog(@"own: order=0 result=%d rank %ld->%ld | order=2 result=%d rank->%ld | order=3 result=%d",
              out_result, (long)before_out, (long)after_out, in_result, (long)after_in, invalid_result);
        if (finder_front && finder_back) {
            // Calibration for "does a no-op return 1000 too?": if the change
            // succeeds, restore with the inverse, which is the same call class.
            attempt(@"foreign above foreign (already above)", finder_front, 1, finder_back);
            NSInteger before = rank(finder_back);
            NSInteger front_rank = rank(finder_front);
            if (before > 0 && front_rank >= 0 && before == front_rank + 1) {
                attempt(@"foreign above foreign (real change)", finder_back, 1, finder_front);
                if (rank(finder_back) < rank(finder_front)) attempt(@"restore foreign order", finder_front, 1, finder_back);
            }
        }

        // AXRaise mutates the target application's own key/main window, so it is
        // opt-in: PROBE_AX_RAISE=1 clang-run when the desktop may be disturbed.
        if (getenv("PROBE_AX_RAISE") && finder_back) {
            pid_t finder_pid = 0;
            NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
            for (NSDictionary *window in windows) {
                if ([window[(id)kCGWindowNumber] unsignedIntValue] == finder_back) {
                    finder_pid = [window[(id)kCGWindowOwnerPID] intValue];
                }
            }
            if (finder_pid) attempt_ax_raise(finder_back, finder_pid);
        }

        [own_a orderOut:nil];
        [own_b orderOut:nil];
        if (original_front && ![original_front isEqual:NSWorkspace.sharedWorkspace.frontmostApplication]) {
            [original_front activateWithOptions:0];
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.3]];
        }
        NSLog(@"final: %@", state());
    }
    return 0;
}
