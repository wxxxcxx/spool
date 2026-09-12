// Renders the bar's toolbar buttons the way src/bar/appkit.rs configures them,
// so the result can be inspected without a screen capture.
#import <AppKit/AppKit.h>

static CGFloat buttonSize(CGFloat band) { CGFloat s = band - 4; if (s < 14) s = 14; if (s > 24) s = 24; return s; }
static CGFloat pointSizeFor(CGFloat band) { CGFloat p = buttonSize(band) * 0.62; if (p < 11) p = 11; if (p > 16) p = 16; return p; }

static NSImage *symbol(NSString *name, CGFloat pt, NSFontWeight weight) {
    NSImage *base = [NSImage imageWithSystemSymbolName:name accessibilityDescription:nil];
    if (!base) return nil;
    NSImageSymbolConfiguration *config = [NSImageSymbolConfiguration configurationWithPointSize:pt weight:weight];
    return [base imageWithSymbolConfiguration:config] ?: base;
}

static void drawPair(NSArray<NSString *> *names, CGFloat x, CGFloat y, CGFloat size, CGFloat pt, NSFontWeight weight, CGFloat alpha, BOOL bezel, BOOL highlightFirst) {
    CGFloat step = size + 6;
    for (NSUInteger i = 0; i < names.count; i++) {
        NSRect rect = NSMakeRect(x + (CGFloat)i * step, y, size, size);
        if (highlightFirst && i == 0) {
            [[NSColor colorWithCalibratedWhite:1.0 alpha:0.12] setFill];
            [[NSBezierPath bezierPathWithRoundedRect:rect xRadius:6 yRadius:6] fill];
        }
        NSImage *image = symbol(names[i], pt, weight);
        if (image) {
            image.template = YES;
            [[NSColor colorWithCalibratedWhite:1.0 alpha:alpha] set];
            [image drawInRect:rect fromRect:NSZeroRect operation:NSCompositingOperationSourceOver fraction:1.0];
        }
        if (bezel) {
            [[NSColor colorWithCalibratedWhite:1.0 alpha:0.35] setStroke];
            NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(rect, 2, 2) xRadius:4 yRadius:4];
            path.lineWidth = 1;
            [path stroke];
        }
    }
}

int main(void) {
    @autoreleasepool {
        NSArray<NSString *> *names = @[@"rectangle.3.group", @"menubar.dock.rectangle"];
        CGFloat band = 34;
        CGFloat size = buttonSize(band);
        CGFloat pt = pointSizeFor(band);
        CGFloat rowHeight = 46, width = 470;
        NSArray<NSString *> *labels = @[
            [NSString stringWithFormat:@"current   weight .medium, %.1fpt   (what ships)", pt],
            [NSString stringWithFormat:@"lighter   weight .regular, %.1fpt", pt],
            @"smaller   weight .medium, 13pt",
            @"before    raw symbol stretched into the button + bezel",
            @"current with the hover highlight (radius 6, white 0.12)",
        ];
        CGFloat height = rowHeight * (CGFloat)labels.count + 16;

        NSBitmapImageRep *rep = [[NSBitmapImageRep alloc]
            initWithBitmapDataPlanes:NULL pixelsWide:(NSInteger)width pixelsHigh:(NSInteger)height
            bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO
            colorSpaceName:NSCalibratedRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
        NSGraphicsContext *ctx = [NSGraphicsContext graphicsContextWithBitmapImageRep:rep];
        [NSGraphicsContext saveGraphicsState];
        [NSGraphicsContext setCurrentContext:ctx];

        [[NSColor colorWithCalibratedRed:0.07 green:0.07 blue:0.08 alpha:1.0] setFill];
        NSRectFill(NSMakeRect(0, 0, width, height));

        NSDictionary *attrs = @{ NSFontAttributeName: [NSFont systemFontOfSize:10],
                                 NSForegroundColorAttributeName: [NSColor colorWithCalibratedWhite:1.0 alpha:0.65] };
        for (NSUInteger i = 0; i < labels.count; i++) {
            CGFloat y = height - rowHeight * (CGFloat)(i + 1);
            [labels[i] drawAtPoint:NSMakePoint(14, y + rowHeight - 16) withAttributes:attrs];
            if (i == 0) drawPair(names, 14, y + 4, size, pt, NSFontWeightMedium, 0.92, NO, NO);
            else if (i == 1) drawPair(names, 14, y + 4, size, pt, NSFontWeightRegular, 0.92, NO, NO);
            else if (i == 2) drawPair(names, 14, y + 4, size, 13, NSFontWeightMedium, 0.92, NO, NO);
            else if (i == 3) drawPair(names, 14, y + 4, size, 0, 0, 0.78, YES, NO);
            else drawPair(names, 14, y + 4, size, pt, NSFontWeightMedium, 0.92, NO, YES);
        }

        [NSGraphicsContext restoreGraphicsState];
        NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        if (![png writeToFile:@"/tmp/barproto/icons/icons.png" atomically:YES]) {
            fprintf(stderr, "write failed\n");
            return 1;
        }
        printf("wrote icons.png\n");
    }
    return 0;
}
