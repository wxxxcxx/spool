// Capability probe: preserve the existing Finder order; never request AX focus.
#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>
#include <dlfcn.h>

int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        void *library = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
        int (*connection)(void) = dlsym(library, "SLSMainConnectionID");
        CGError (*order)(int, uint32_t, int, uint32_t) = dlsym(library, "SLSOrderWindow");
        if (!connection || !order) return 2;
        NSWindow *own = [[NSWindow alloc] initWithContentRect:NSMakeRect(-10000, -10000, 20, 20)
            styleMask:NSWindowStyleMaskBorderless backing:NSBackingStoreBuffered defer:NO];
        CGError ownResult = order(connection(), (uint32_t)own.windowNumber, 1, 0);
        [own orderOut:nil];
        NSLog(@"own_window_order_result=%d", ownResult);
        NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
        NSMutableArray *finder = [NSMutableArray array];
        for (NSDictionary *window in windows) {
            pid_t pid = [window[(id)kCGWindowOwnerPID] intValue];
            NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
            if ([app.bundleIdentifier isEqual:@"com.apple.finder"] &&
                [window[(id)kCGWindowLayer] intValue] == 0) {
                [finder addObject:window[(id)kCGWindowNumber]];
            }
        }
        if (finder.count < 2) {
            NSLog(@"need_two_visible_finder_windows; found=%@", finder);
            return 3;
        }
        uint32_t front = [finder[0] unsignedIntValue];
        uint32_t back = [finder[1] unsignedIntValue];
        CGError result = order(connection(), front, 1, back);
        NSLog(@"finder_front=%u finder_back=%u preserve_order_result=%d", front, back, result);
    }
    return 0;
}
