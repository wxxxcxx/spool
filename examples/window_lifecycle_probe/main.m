// PROTOTYPE: Compare AX, CGWindow, and SkyLight lifecycle signals for one window.
#import <Cocoa/Cocoa.h>
#import <ApplicationServices/ApplicationServices.h>

#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef int32_t SLSConnectionID;
typedef void (*SLSNotifyProc)(uint32_t, void *, size_t, void *, SLSConnectionID);

extern SLSConnectionID SLSMainConnectionID(void);
extern CGError SLSRegisterConnectionNotifyProc(
    SLSConnectionID connection,
    SLSNotifyProc callback,
    uint32_t event,
    void *context
);
extern AXError _AXUIElementGetWindow(AXUIElementRef element, CGWindowID *window_id);

static const uint32_t kCandidateEvents[] = {
    804,  // WindowClosed
    815,  // WindowUnhidden
    816,  // WindowHidden
    1325, // SpaceWindowCreated
    1326, // SpaceWindowDestroyed
    1402, // WorkspaceWindowIsViewable
    1403, // WorkspaceWindowIsNotViewable
    1416, // WorkspacesWindowDidOrderOutOnNonCurrentManagedSpaces
};

static pid_t gTargetPid;
static _Atomic(uint32_t) gTargetWindow;
static _Atomic(bool) gAxDestroyed;
static _Atomic(uint64_t) gTargetSlsEvents;
static AXUIElementRef gApplication;
static AXUIElementRef gTargetElement;
static AXObserverRef gObserver;
static NSRunningApplication *gRunningApplication;
static NSMutableSet<NSNumber *> *gObservedWindows;
static NSString *gLastState;
static CFAbsoluteTime gUnavailableSince;
static BOOL gVerdictPrinted;
static BOOL gWaitingPrinted;

static const char *event_name(uint32_t event) {
    switch (event) {
        case 804: return "WindowClosed";
        case 815: return "WindowUnhidden";
        case 816: return "WindowHidden";
        case 1325: return "SpaceWindowCreated";
        case 1326: return "SpaceWindowDestroyed";
        case 1402: return "WorkspaceWindowIsViewable";
        case 1403: return "WorkspaceWindowIsNotViewable";
        case 1416: return "WorkspacesWindowDidOrderOutOnNonCurrentManagedSpaces";
        default: return "Unknown";
    }
}

static uint64_t event_bit(uint32_t event) {
    for (size_t i = 0; i < sizeof(kCandidateEvents) / sizeof(kCandidateEvents[0]); i++) {
        if (kCandidateEvents[i] == event) {
            return UINT64_C(1) << i;
        }
    }
    return 0;
}

static CGWindowID event_window_id(uint32_t event, void *data, size_t length) {
    CGWindowID window_id = 0;
    if (data == NULL) {
        return 0;
    }

    size_t offset = (event == 1325 || event == 1326) ? sizeof(uint64_t) : 0;
    if (length >= offset + sizeof(window_id)) {
        memcpy(&window_id, (const uint8_t *)data + offset, sizeof(window_id));
    }
    return window_id;
}

static void sls_callback(
    uint32_t event,
    void *data,
    size_t length,
    void *context,
    SLSConnectionID connection
) {
    (void)context;
    (void)connection;

    CGWindowID event_window = event_window_id(event, data, length);
    CGWindowID target = atomic_load(&gTargetWindow);
    BOOL target_match = target != 0 && event_window == target;
    if (target_match) {
        atomic_fetch_or(&gTargetSlsEvents, event_bit(event));
    }

    printf(
        "SLS event=%u name=%s length=%zu window=%u target_match=%s\n",
        event,
        event_name(event),
        length,
        event_window,
        target_match ? "true" : "false"
    );
    fflush(stdout);
}

static CGWindowID ax_window_id(AXUIElementRef element) {
    CGWindowID window_id = 0;
    if (element == NULL || _AXUIElementGetWindow(element, &window_id) != kAXErrorSuccess) {
        return 0;
    }
    return window_id;
}

static void ax_callback(
    AXObserverRef observer,
    AXUIElementRef element,
    CFStringRef notification,
    void *context
) {
    (void)observer;

    CGWindowID window_id = (CGWindowID)(uintptr_t)context;
    if (window_id == 0) {
        window_id = ax_window_id(element);
    }
    NSString *name = (__bridge NSString *)notification;
    CGWindowID target = atomic_load(&gTargetWindow);
    BOOL target_match = target != 0 && window_id == target;
    if (target_match && [name isEqualToString:@"AXUIElementDestroyed"]) {
        atomic_store(&gAxDestroyed, true);
    }

    printf(
        "AX event=%s window=%u target_match=%s\n",
        name.UTF8String,
        window_id,
        target_match ? "true" : "false"
    );
    fflush(stdout);
}

static void observe_window(AXUIElementRef window, CGWindowID window_id) {
    NSNumber *key = @(window_id);
    if (window_id == 0 || [gObservedWindows containsObject:key]) {
        return;
    }

    AXError result = AXObserverAddNotification(
        gObserver,
        window,
        CFSTR("AXUIElementDestroyed"),
        (void *)(uintptr_t)window_id
    );
    printf("AX register window=%u notification=AXUIElementDestroyed status=%d\n", window_id, result);
    fflush(stdout);
    if (result == kAXErrorSuccess || result == kAXErrorNotificationAlreadyRegistered) {
        [gObservedWindows addObject:key];
    }
}

static NSArray<NSNumber *> *snapshot_ax_windows(void) {
    CFTypeRef value = NULL;
    AXError result = AXUIElementCopyAttributeValue(gApplication, CFSTR("AXWindows"), &value);
    if (result != kAXErrorSuccess || value == NULL || CFGetTypeID(value) != CFArrayGetTypeID()) {
        if (value != NULL) {
            CFRelease(value);
        }
        return @[];
    }

    NSMutableArray<NSNumber *> *ids = [NSMutableArray array];
    CFArrayRef windows = (CFArrayRef)value;
    CFIndex count = CFArrayGetCount(windows);
    CGWindowID target = atomic_load(&gTargetWindow);
    for (CFIndex i = 0; i < count; i++) {
        AXUIElementRef window = (AXUIElementRef)CFArrayGetValueAtIndex(windows, i);
        CGWindowID window_id = ax_window_id(window);
        if (window_id == 0) {
            continue;
        }
        [ids addObject:@(window_id)];
        observe_window(window, window_id);
        if (target != 0 && target == window_id && gTargetElement == NULL) {
            gTargetElement = (AXUIElementRef)CFRetain(window);
        }
    }
    CFRelease(value);
    [ids sortUsingSelector:@selector(compare:)];
    return ids;
}

static AXUIElementRef copy_focused_window(void) {
    CFTypeRef value = NULL;
    AXError result = AXUIElementCopyAttributeValue(
        gApplication,
        CFSTR("AXFocusedWindow"),
        &value
    );
    if (result != kAXErrorSuccess || value == NULL || CFGetTypeID(value) != AXUIElementGetTypeID()) {
        if (value != NULL) {
            CFRelease(value);
        }
        return NULL;
    }
    return (AXUIElementRef)value;
}

static BOOL target_ax_handle_responds(void) {
    if (gTargetElement == NULL) {
        return NO;
    }
    CFTypeRef role = NULL;
    AXError result = AXUIElementCopyAttributeValue(gTargetElement, CFSTR("AXRole"), &role);
    if (role != NULL) {
        CFRelease(role);
    }
    return result == kAXErrorSuccess;
}

static NSString *format_ids(NSArray<NSNumber *> *ids) {
    NSMutableArray<NSString *> *values = [NSMutableArray arrayWithCapacity:ids.count];
    for (NSNumber *window_id in ids) {
        [values addObject:window_id.stringValue];
    }
    return [NSString stringWithFormat:@"[%@]", [values componentsJoinedByString:@","]];
}

static NSString *snapshot_cg_windows(BOOL *target_present, BOOL *target_onscreen) {
    *target_present = NO;
    *target_onscreen = NO;
    CGWindowID target = atomic_load(&gTargetWindow);
    CFArrayRef raw = CGWindowListCopyWindowInfo(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID
    );
    NSArray<NSDictionary *> *windows = CFBridgingRelease(raw);
    NSMutableArray<NSString *> *values = [NSMutableArray array];

    for (NSDictionary *window in windows) {
        NSNumber *owner_pid = window[(__bridge NSString *)kCGWindowOwnerPID];
        if (owner_pid.intValue != gTargetPid) {
            continue;
        }

        NSNumber *number = window[(__bridge NSString *)kCGWindowNumber];
        NSNumber *onscreen = window[(__bridge NSString *)kCGWindowIsOnscreen];
        NSNumber *layer = window[(__bridge NSString *)kCGWindowLayer];
        CGWindowID window_id = number.unsignedIntValue;
        BOOL is_onscreen = onscreen.boolValue;
        [values addObject:[NSString stringWithFormat:@"%u:%@/L%@", window_id,
                                                       is_onscreen ? @"on" : @"off", layer]];
        if (target != 0 && window_id == target) {
            *target_present = YES;
            *target_onscreen = is_onscreen;
        }
    }
    [values sortUsingSelector:@selector(localizedStandardCompare:)];
    return [NSString stringWithFormat:@"[%@]", [values componentsJoinedByString:@","]];
}

static void print_verdict_if_ready(BOOL unavailable) {
    if (!unavailable) {
        if (gUnavailableSince != 0 && gVerdictPrinted) {
            printf("RESULT TARGET_RETURNED_WITH_SAME_ID window=%u\n", atomic_load(&gTargetWindow));
            fflush(stdout);
        }
        gUnavailableSince = 0;
        gVerdictPrinted = NO;
        return;
    }

    if (gUnavailableSince == 0) {
        gUnavailableSince = CFAbsoluteTimeGetCurrent();
        printf("TRANSITION target became unavailable; waiting 1s for delayed notifications\n");
        fflush(stdout);
        return;
    }
    if (gVerdictPrinted || CFAbsoluteTimeGetCurrent() - gUnavailableSince < 1.0) {
        return;
    }

    BOOL ax_destroyed = atomic_load(&gAxDestroyed);
    uint64_t sls_events = atomic_load(&gTargetSlsEvents);
    BOOL sls_destroyed = (sls_events & (event_bit(804) | event_bit(1326))) != 0;
    if (!ax_destroyed && !sls_destroyed) {
        printf(
            "RESULT CONFIRMED_WITHDRAWN_WITHOUT_DESTROY_EVENT window=%u "
            "ax_destroyed=false sls_target_events=0x%llx ax_handle_responds=%s\n",
            atomic_load(&gTargetWindow),
            (unsigned long long)sls_events,
            target_ax_handle_responds() ? "true" : "false"
        );
    } else {
        printf(
            "RESULT DESTROY_EVENT_OBSERVED window=%u ax_destroyed=%s sls_target_events=0x%llx\n",
            atomic_load(&gTargetWindow),
            ax_destroyed ? "true" : "false",
            (unsigned long long)sls_events
        );
    }
    fflush(stdout);
    gVerdictPrinted = YES;
}

static void tick(void) {
    NSArray<NSNumber *> *ax_ids = snapshot_ax_windows();
    CGWindowID target = atomic_load(&gTargetWindow);

    if (target == 0) {
        if (!gRunningApplication.active) {
            if (!gWaitingPrinted) {
                printf("WAITING activate WeChat and leave its main window open\n");
                fflush(stdout);
                gWaitingPrinted = YES;
            }
            return;
        }

        AXUIElementRef focused = copy_focused_window();
        target = ax_window_id(focused);
        if (target == 0) {
            if (focused != NULL) {
                CFRelease(focused);
            }
            return;
        }
        gTargetElement = focused;
        atomic_store(&gTargetWindow, target);
        printf("TARGET pid=%d window=%u\n", gTargetPid, target);
        fflush(stdout);
    }

    BOOL ax_present = [ax_ids containsObject:@(target)];
    BOOL cg_present = NO;
    BOOL cg_onscreen = NO;
    NSString *cg_windows = snapshot_cg_windows(&cg_present, &cg_onscreen);
    BOOL handle_responds = target_ax_handle_responds();
    NSString *state = [NSString stringWithFormat:
        @"target=%u ax_present=%@ cg_present=%@ cg_onscreen=%@ ax_handle_responds=%@ ax=%@ cg=%@",
        target,
        ax_present ? @"true" : @"false",
        cg_present ? @"true" : @"false",
        cg_onscreen ? @"true" : @"false",
        handle_responds ? @"true" : @"false",
        format_ids(ax_ids),
        cg_windows
    ];
    if (![state isEqualToString:gLastState]) {
        printf("STATE %s\n", state.UTF8String);
        fflush(stdout);
        gLastState = state;
    }

    BOOL unavailable = !ax_present && (!cg_present || !cg_onscreen);
    print_verdict_if_ready(unavailable);
}

static NSRunningApplication *find_application(NSString *bundle_id) {
    NSArray<NSRunningApplication *> *exact =
        [NSRunningApplication runningApplicationsWithBundleIdentifier:bundle_id];
    if (exact.count > 0) {
        return exact.firstObject;
    }

    for (NSRunningApplication *application in NSWorkspace.sharedWorkspace.runningApplications) {
        NSString *name = application.localizedName.lowercaseString;
        NSString *identifier = application.bundleIdentifier.lowercaseString;
        if ([name containsString:@"wechat"] || [name containsString:@"weixin"] ||
            [identifier containsString:@"wechat"] || [identifier containsString:@"weixin"]) {
            return application;
        }
    }
    return nil;
}

static void usage(const char *program) {
    printf(
        "Usage: %s [--pid PID] [--window-id ID] [--bundle-id BUNDLE_ID]\n"
        "\n"
        "Default bundle id: com.tencent.xinWeChat\n"
        "Without --window-id, activate WeChat after startup; the probe locks onto its focused window.\n",
        program
    );
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        pid_t requested_pid = 0;
        CGWindowID requested_window = 0;
        NSString *bundle_id = @"com.tencent.xinWeChat";
        for (int i = 1; i < argc; i++) {
            if (strcmp(argv[i], "--help") == 0 || strcmp(argv[i], "-h") == 0) {
                usage(argv[0]);
                return 0;
            }
            if (i + 1 >= argc) {
                usage(argv[0]);
                return 2;
            }
            if (strcmp(argv[i], "--pid") == 0) {
                requested_pid = (pid_t)strtol(argv[++i], NULL, 10);
            } else if (strcmp(argv[i], "--window-id") == 0) {
                requested_window = (CGWindowID)strtoul(argv[++i], NULL, 10);
            } else if (strcmp(argv[i], "--bundle-id") == 0) {
                bundle_id = [NSString stringWithUTF8String:argv[++i]];
            } else {
                usage(argv[0]);
                return 2;
            }
        }

        NSDictionary *trust_options = @{
            (__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES,
        };
        if (!AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)trust_options)) {
            fprintf(stderr, "Accessibility permission is required; grant it, then run again.\n");
            return 1;
        }

        gRunningApplication = find_application(bundle_id);
        if (requested_pid == 0 && gRunningApplication == nil) {
            fprintf(stderr, "WeChat is not running (bundle id: %s).\n", bundle_id.UTF8String);
            return 1;
        }
        gTargetPid = requested_pid != 0 ? requested_pid : gRunningApplication.processIdentifier;
        if (gRunningApplication == nil) {
            gRunningApplication = [NSRunningApplication runningApplicationWithProcessIdentifier:gTargetPid];
        }
        atomic_store(&gTargetWindow, requested_window);
        atomic_store(&gAxDestroyed, false);
        atomic_store(&gTargetSlsEvents, 0);
        gObservedWindows = [NSMutableSet set];
        gApplication = AXUIElementCreateApplication(gTargetPid);
        AXUIElementSetMessagingTimeout(gApplication, 0.25);

        AXError observer_status = AXObserverCreate(gTargetPid, ax_callback, &gObserver);
        if (observer_status != kAXErrorSuccess) {
            fprintf(stderr, "AXObserverCreate failed: %d\n", observer_status);
            return 1;
        }
        CFRunLoopAddSource(
            CFRunLoopGetCurrent(),
            AXObserverGetRunLoopSource(gObserver),
            kCFRunLoopCommonModes
        );
        AXObserverAddNotification(gObserver, gApplication, CFSTR("AXCreated"), NULL);
        AXObserverAddNotification(gObserver, gApplication, CFSTR("AXFocusedWindowChanged"), NULL);

        SLSConnectionID connection = SLSMainConnectionID();
        for (size_t i = 0; i < sizeof(kCandidateEvents) / sizeof(kCandidateEvents[0]); i++) {
            uint32_t event = kCandidateEvents[i];
            CGError status = SLSRegisterConnectionNotifyProc(
                connection,
                sls_callback,
                event,
                NULL
            );
            printf("SLS register event=%u name=%s status=%d\n", event, event_name(event), status);
        }
        fflush(stdout);

        printf("PROBE pid=%d bundle_id=%s\n", gTargetPid, bundle_id.UTF8String);
        if (requested_window != 0) {
            printf("TARGET pid=%d window=%u (explicit)\n", gTargetPid, requested_window);
        }
        fflush(stdout);

        [NSTimer scheduledTimerWithTimeInterval:0.25
                                         repeats:YES
                                           block:^(NSTimer *timer) {
                                               (void)timer;
                                               tick();
                                           }];
        [NSRunLoop.currentRunLoop run];
    }
    return 0;
}
