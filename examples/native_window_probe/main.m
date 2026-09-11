// Read-only diagnostic: compare AX window identities with WindowServer surfaces.
#import <Cocoa/Cocoa.h>
#import <ApplicationServices/ApplicationServices.h>

extern AXError _AXUIElementGetWindow(AXUIElementRef element, CGWindowID *window_id);

static id attribute(AXUIElementRef element, CFStringRef name) {
    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(element, name, &value);
    if (error != kAXErrorSuccess) {
        if (value) CFRelease(value);
        return @{ @"error": @(error) };
    }
    return value ? CFBridgingRelease(value) : [NSNull null];
}

static NSNumber *window_id(AXUIElementRef element) {
    CGWindowID value = 0;
    _AXUIElementGetWindow(element, &value);
    return @(value);
}

static id frame_value(AXUIElementRef element, CFStringRef name, AXValueType type) {
    id value = attribute(element, name);
    if (CFGetTypeID((__bridge CFTypeRef)value) != AXValueGetTypeID()) return value;
    if (type == kAXValueCGPointType) {
        CGPoint point = CGPointZero;
        if (AXValueGetValue((__bridge AXValueRef)value, type, &point))
            return @[ @(point.x), @(point.y) ];
    } else {
        CGSize size = CGSizeZero;
        if (AXValueGetValue((__bridge AXValueRef)value, type, &size))
            return @[ @(size.width), @(size.height) ];
    }
    return [NSNull null];
}

static NSDictionary *identity(AXUIElementRef element) {
    return @{ @"id": window_id(element), @"ax_hash": @(CFHash(element)),
              @"role": attribute(element, kAXRoleAttribute),
              @"subrole": attribute(element, kAXSubroleAttribute) };
}

static id relationship(AXUIElementRef element, CFStringRef name) {
    id value = attribute(element, name);
    if (CFGetTypeID((__bridge CFTypeRef)value) == AXUIElementGetTypeID())
        return identity((__bridge AXUIElementRef)value);
    return value;
}

static NSDictionary *window_info(AXUIElementRef element) {
    NSMutableDictionary *info = [identity(element) mutableCopy];
    info[@"position"] = frame_value(element, kAXPositionAttribute, kAXValueCGPointType);
    info[@"size"] = frame_value(element, kAXSizeAttribute, kAXValueCGSizeType);
    for (NSString *name in @[ @"AXMinimized", @"AXMain", @"AXFocused", @"AXFullScreen" ])
        info[name] = attribute(element, (__bridge CFStringRef)name);
    info[@"parent"] = relationship(element, kAXParentAttribute);
    info[@"window"] = relationship(element, kAXWindowAttribute);
    NSMutableArray *children = [NSMutableArray array];
    id raw = attribute(element, kAXChildrenAttribute);
    if ([raw isKindOfClass:[NSArray class]]) {
        for (id child in raw) {
            AXUIElementRef ref = (__bridge AXUIElementRef)child;
            NSMutableDictionary *item = [identity(ref) mutableCopy];
            item[@"window"] = relationship(ref, kAXWindowAttribute);
            if ([item[@"role"] isEqual:@"AXTabGroup"]) {
                id tabs = attribute(ref, kAXTabsAttribute);
                if ([tabs isKindOfClass:[NSArray class]]) {
                    NSMutableArray *members = [NSMutableArray array];
                    for (id tab in tabs) {
                        AXUIElementRef t = (__bridge AXUIElementRef)tab;
                        NSMutableDictionary *member = [identity(t) mutableCopy];
                        member[@"window"] = relationship(t, kAXWindowAttribute);
                        member[@"value"] = attribute(t, kAXValueAttribute);
                        [members addObject:member];
                    }
                    item[@"tabs"] = members;
                } else item[@"tabs"] = tabs;
            }
            [children addObject:item];
        }
    }
    info[@"children"] = children;
    CFArrayRef names = NULL;
    if (AXUIElementCopyAttributeNames(element, &names) == kAXErrorSuccess && names)
        info[@"attributes"] = CFBridgingRelease(names);
    return info;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 2 && argc != 3) {
            fprintf(stderr, "usage: native-window-probe bundle-id|pid:123 [hold-seconds]\n");
            return 2;
        }
        NSString *target = [NSString stringWithUTF8String:argv[1]];
        NSArray *apps;
        if ([target hasPrefix:@"pid:"]) {
            NSRunningApplication *selected = [NSRunningApplication
                runningApplicationWithProcessIdentifier:[[target substringFromIndex:4] intValue]];
            apps = selected ? @[selected] : @[];
        } else apps = [NSRunningApplication runningApplicationsWithBundleIdentifier:target];
        if (apps.count != 1) {
            fprintf(stderr, "expected one running app, matched %lu; use pid:<number>\n",
                (unsigned long)apps.count);
            return 2;
        }
        NSRunningApplication *app = apps.firstObject;
        AXUIElementRef ax = AXUIElementCreateApplication(app.processIdentifier);
        AXUIElementSetMessagingTimeout(ax, 1.0f);
        NSMutableDictionary *result = [@{ @"bundle": app.bundleIdentifier,
            @"pid": @(app.processIdentifier), @"trusted": @(AXIsProcessTrusted()),
            @"focused": relationship(ax, kAXFocusedWindowAttribute) } mutableCopy];
        id raw = attribute(ax, kAXWindowsAttribute);
        NSMutableArray *windows = [NSMutableArray array];
        if ([raw isKindOfClass:[NSArray class]]) {
            for (id window in raw) [windows addObject:window_info((__bridge AXUIElementRef)window)];
            result[@"ax_windows"] = windows;
        } else result[@"ax_windows"] = raw;
        if (argc == 3 && [raw isKindOfClass:[NSArray class]]) {
            double seconds = strtod(argv[2], NULL);
            if (!(seconds > 0 && seconds <= 60)) { CFRelease(ax); return 2; }
            NSMutableArray *anchors = [NSMutableArray array];
            for (id window in raw) {
                id children = attribute((__bridge AXUIElementRef)window, kAXChildrenAttribute);
                if (![children isKindOfClass:[NSArray class]]) continue;
                for (id child in children) {
                    if ([attribute((__bridge AXUIElementRef)child, kAXRoleAttribute) isEqual:@"AXTabGroup"])
                        [anchors addObject:child];
                }
            }
            fprintf(stderr, "holding AX handles for %.1f seconds\n", seconds);
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, false);
            NSMutableArray *held = [NSMutableArray array];
            for (id window in raw) [held addObject:window_info((__bridge AXUIElementRef)window)];
            result[@"held_windows"] = held;
            NSMutableArray *owners = [NSMutableArray array];
            for (id anchor in anchors) {
                AXUIElementRef ref = (__bridge AXUIElementRef)anchor;
                [owners addObject:@{ @"anchor": identity(ref),
                    @"window": relationship(ref, kAXWindowAttribute) }];
            }
            result[@"retained_anchor_owners"] = owners;
        }
        CFRelease(ax);
        NSArray *server = CFBridgingRelease(CGWindowListCopyWindowInfo(
            kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
        NSMutableArray *surfaces = [NSMutableArray array];
        for (NSDictionary *surface in server) {
            if ([surface[(id)kCGWindowOwnerPID] intValue] != app.processIdentifier) continue;
            NSMutableDictionary *item = [NSMutableDictionary dictionary];
            for (NSString *key in @[ (id)kCGWindowNumber, (id)kCGWindowLayer,
                    (id)kCGWindowAlpha, (id)kCGWindowBounds, (id)kCGWindowIsOnscreen ])
                item[key] = surface[key] ?: [NSNull null];
            [surfaces addObject:item];
        }
        result[@"cg_windows"] = server ? surfaces : (id)[NSNull null];
        NSError *error = nil;
        NSData *json = [NSJSONSerialization dataWithJSONObject:result
            options:NSJSONWritingPrettyPrinted | NSJSONWritingSortedKeys error:&error];
        if (!json) { fprintf(stderr, "%s\n", error.description.UTF8String); return 1; }
        fwrite(json.bytes, 1, json.length, stdout);
        puts("");
    }
    return 0;
}
