// Read-only diagnostic: where the Spool Bar panels are, whether they are on
// screen, and what else lives at menu-bar level or above.
//
// The question this answers: something hides the Bar on one display but not
// another (reported with Show Desktop on a notched display). A window that the
// window server has hidden disappears from the on-screen list; one that was
// merely moved aside keeps its listing and moves; one that is still listed and
// still where it belongs is being drawn invisibly, which is the Bar's own
// business rather than the window server's.
//
// Build: clang -fobjc-arc -framework Cocoa -framework CoreGraphics \
//          -o /tmp/bar_windows bar_windows.m
// Run:   /tmp/bar_windows [label]
#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>

static NSArray<NSDictionary *> *windows(CGWindowListOption option) {
    return CFBridgingRelease(CGWindowListCopyWindowInfo(
        option | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
}

static void report(NSString *label, NSArray<NSDictionary *> *onScreen,
                   NSArray<NSDictionary *> *all) {
    NSMutableSet<NSNumber *> *visible = [NSMutableSet set];
    for (NSDictionary *window in onScreen) [visible addObject:window[(id)kCGWindowNumber]];

    printf("── %s ──\n", label.UTF8String);
    printf("spool windows (all):\n");
    int spoolCount = 0;
    for (NSDictionary *window in all) {
        NSString *owner = window[(id)kCGWindowOwnerName];
        if (![owner isEqualToString:@"spool"]) continue;
        spoolCount++;
        NSDictionary *bounds = window[(id)kCGWindowBounds];
        printf("  number=%4d layer=%4d onscreen=%d alpha=%.2f bounds=(%.0f,%.0f %.0fx%.0f)\n",
               [window[(id)kCGWindowNumber] intValue],
               [window[(id)kCGWindowLayer] intValue],
               [visible containsObject:window[(id)kCGWindowNumber]],
               [window[(id)kCGWindowAlpha] doubleValue],
               [bounds[@"X"] doubleValue], [bounds[@"Y"] doubleValue],
               [bounds[@"Width"] doubleValue], [bounds[@"Height"] doubleValue]);
    }
    if (spoolCount == 0) printf("  (none)\n");

    printf("normal windows (layer 0), on screen, first six:\n");
    int shown = 0;
    for (NSDictionary *window in onScreen) {
        if ([window[(id)kCGWindowLayer] intValue] != 0) continue;
        if (shown++ >= 6) break;
        NSDictionary *bounds = window[(id)kCGWindowBounds];
        printf("  %-24s alpha=%.2f bounds=(%.0f,%.0f %.0fx%.0f)\n",
               [window[(id)kCGWindowOwnerName] UTF8String],
               [window[(id)kCGWindowAlpha] doubleValue],
               [bounds[@"X"] doubleValue], [bounds[@"Y"] doubleValue],
               [bounds[@"Width"] doubleValue], [bounds[@"Height"] doubleValue]);
    }
    printf("everything at layer >= 24, on screen:\n");
    for (NSDictionary *window in onScreen) {
        int layer = [window[(id)kCGWindowLayer] intValue];
        if (layer < 24) continue;
        NSDictionary *bounds = window[(id)kCGWindowBounds];
        printf("  %-28s layer=%4d number=%6d bounds=(%.0f,%.0f %.0fx%.0f)\n",
               [window[(id)kCGWindowOwnerName] UTF8String], layer,
               [window[(id)kCGWindowNumber] intValue],
               [bounds[@"X"] doubleValue], [bounds[@"Y"] doubleValue],
               [bounds[@"Width"] doubleValue], [bounds[@"Height"] doubleValue]);
    }
    printf("screens (AppKit frame, y up):\n");
    for (NSScreen *screen in [NSScreen screens]) {
        NSRect frame = screen.frame;
        NSRect visible = screen.visibleFrame;
        printf("  frame=(%.0f,%.0f %.0fx%.0f) visible=(%.0f,%.0f %.0fx%.0f) safeTop=%.0f\n",
               frame.origin.x, frame.origin.y, frame.size.width, frame.size.height,
               visible.origin.x, visible.origin.y, visible.size.width, visible.size.height,
               screen.safeAreaInsets.top);
    }
    fflush(stdout);
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        NSString *label = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : @"probe";
        report(label, windows(kCGWindowListOptionOnScreenOnly), windows(kCGWindowListOptionAll));
    }
    return 0;
}
