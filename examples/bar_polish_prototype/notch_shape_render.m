// Draws the collapsed chrome exactly as chrome_path() computes it, at 4x, so the
// shoulder can be compared with the photograph instead of with an HTML mock.
#import <AppKit/AppKit.h>

static const CGFloat KAPPA = 0.5522847498;

static NSBezierPath *chromePath(NSRect rect, CGFloat bottom, CGFloat top, BOOL concaveTop) {
    CGFloat left = NSMinX(rect), right = NSMaxX(rect), ty = NSMinY(rect), by = NSMaxY(rect);
    CGFloat limit = MIN(rect.size.width / 2.0, rect.size.height);
    BOOL overhang = top < 0;
    CGFloat b = MIN(bottom, limit), t = MIN(fabs(top), limit);
    NSBezierPath *path = [NSBezierPath bezierPath];
    if (overhang) {
        [path moveToPoint:NSMakePoint(left - t, ty)];
        [path lineToPoint:NSMakePoint(right + t, ty)];
        [path curveToPoint:NSMakePoint(right, ty + t)
             controlPoint1:NSMakePoint(right + t - t * KAPPA, ty)
             controlPoint2:NSMakePoint(right, ty + t - t * KAPPA)];
    } else if (concaveTop && t > 0) {
        [path moveToPoint:NSMakePoint(left - t, ty)];
        [path lineToPoint:NSMakePoint(right + t, ty)];
        [path curveToPoint:NSMakePoint(right, ty + t)
             controlPoint1:NSMakePoint(right + t, ty + t * KAPPA)
             controlPoint2:NSMakePoint(right - t * KAPPA, ty + t)];
    } else {
        [path moveToPoint:NSMakePoint(left + t, ty)];
        [path lineToPoint:NSMakePoint(right - t, ty)];
        if (t > 0) {
            [path curveToPoint:NSMakePoint(right, ty + t)
                 controlPoint1:NSMakePoint(right - t + t * KAPPA, ty)
                 controlPoint2:NSMakePoint(right, ty + t - t * KAPPA)];
        } else {
            [path lineToPoint:NSMakePoint(right, ty)];
        }
    }
    if (b > 0) {
        [path lineToPoint:NSMakePoint(right, by - b)];
        [path curveToPoint:NSMakePoint(right - b, by)
             controlPoint1:NSMakePoint(right, by - b + b * KAPPA)
             controlPoint2:NSMakePoint(right - b + b * KAPPA, by)];
        [path lineToPoint:NSMakePoint(left + b, by)];
        [path curveToPoint:NSMakePoint(left, by - b)
             controlPoint1:NSMakePoint(left + b - b * KAPPA, by)
             controlPoint2:NSMakePoint(left, by - b + b * KAPPA)];
    } else {
        [path lineToPoint:NSMakePoint(right, by)];
        [path lineToPoint:NSMakePoint(left, by)];
    }
    if (overhang) {
        [path lineToPoint:NSMakePoint(left, ty + t)];
        [path curveToPoint:NSMakePoint(left - t, ty)
             controlPoint1:NSMakePoint(left, ty + t - t * KAPPA)
             controlPoint2:NSMakePoint(left - t + t * KAPPA, ty)];
    } else if (concaveTop && t > 0) {
        [path lineToPoint:NSMakePoint(left, ty + t)];
        [path curveToPoint:NSMakePoint(left - t, ty)
             controlPoint1:NSMakePoint(left + t * KAPPA, ty + t)
             controlPoint2:NSMakePoint(left - t, ty + t * KAPPA)];
    } else {
        [path lineToPoint:NSMakePoint(left, ty + t)];
        if (t > 0) {
            [path curveToPoint:NSMakePoint(left + t, ty)
                 controlPoint1:NSMakePoint(left, ty + t - t * KAPPA)
                 controlPoint2:NSMakePoint(left + t - t * KAPPA, ty)];
        } else {
            [path lineToPoint:NSMakePoint(left, ty)];
        }
    }
    [path closePath];
    return path;
}

// The hover halo: a shape layer whose path is the collapsed outline, stroked
// white, with the bloom cast from the same path. `phase` picks the two ends of
// the breath (GLOW_PULSE / GLOW_SWELL in appkit.rs).
static void halo(NSBezierPath *path, CGFloat radius, CGFloat lineWidth, CGFloat layerOpacity, CGFloat phase) {
    // sync_glow: rim = white 0.55 * layer opacity, bloom = white * layer opacity.
    NSShadow *shadow = [[NSShadow alloc] init];
    shadow.shadowColor = [[NSColor whiteColor] colorWithAlphaComponent:layerOpacity];
    shadow.shadowBlurRadius = radius;
    shadow.shadowOffset = NSMakeSize(0, 0);
    [NSGraphicsContext saveGraphicsState];
    [shadow set];
    [[[NSColor whiteColor] colorWithAlphaComponent:0.55 * layerOpacity] setStroke];
    path.lineWidth = lineWidth;
    [path stroke];
    [NSGraphicsContext restoreGraphicsState];
    (void)phase;
}

// One panel: a wallpaper-ish background with the shape hanging from the top edge.
static void panel(NSRect frame, NSRect shape, CGFloat bottom, CGFloat top, BOOL concave, CGFloat scale, NSString *label, NSDictionary *attrs, CGFloat height, CGFloat haloAlpha, CGFloat haloRadius) {
    [[NSColor colorWithCalibratedRed:0.72 green:0.45 blue:0.72 alpha:1.0] setFill];
    NSRectFill(frame);
    // the screen's top edge
    [[NSColor colorWithCalibratedWhite:0.35 alpha:1.0] setFill];
    NSRectFill(NSMakeRect(frame.origin.x, NSMinY(frame), frame.size.width, 3 * scale));
    NSBezierPath *path = chromePath(shape, bottom * scale, top * scale, concave);
    [[NSColor blackColor] setFill];
    [path fill];
    if (haloAlpha > 0) halo(path, haloRadius * scale, 1.2 * scale, haloAlpha, 0);
    // Undo the flip for the text so the captions read the right way up.
    [NSGraphicsContext saveGraphicsState];
    NSAffineTransform *unflip = [NSAffineTransform transform];
    [unflip translateXBy:0 yBy:height];
    [unflip scaleXBy:1 yBy:-1];
    [unflip concat];
    [label drawAtPoint:NSMakePoint(frame.origin.x, height - (NSMaxY(frame) + 20)) withAttributes:attrs];
    [NSGraphicsContext restoreGraphicsState];
}

int main(void) {
    @autoreleasepool {
        CGFloat scale = 4;
        CGFloat pad = 20;
        CGFloat panelW = 300 * scale, panelH = 60 * scale;
        NSArray *labels = @[@"collapsed capsule, no hover",
                            @"hover halo, quiet end (layer 0.30, bloom 0.60x = 5.4pt)",
                            @"hover halo, peak (layer 0.75, bloom 1.35x = 12pt)  <- softened after this render"];
        CGFloat width = panelW + pad * 2;
        CGFloat height = (panelH + 40) * 3 + pad;

        NSBitmapImageRep *rep = [[NSBitmapImageRep alloc]
            initWithBitmapDataPlanes:NULL pixelsWide:(NSInteger)width pixelsHigh:(NSInteger)height
            bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO
            colorSpaceName:NSCalibratedRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
        NSGraphicsContext *ctx = [NSGraphicsContext graphicsContextWithBitmapImageRep:rep];
        [NSGraphicsContext saveGraphicsState];
        [NSGraphicsContext setCurrentContext:ctx];
        // The Bar's view is flipped: chrome_path works in y-down coordinates, so
        // the harness has to as well or the shape renders upside down.
        NSAffineTransform *flip = [NSAffineTransform transform];
        [flip translateXBy:0 yBy:height];
        [flip scaleXBy:1 yBy:-1];
        [flip concat];
        [[NSColor colorWithCalibratedWhite:0.06 alpha:1.0] setFill];
        NSRectFill(NSMakeRect(0, 0, width, height));
        NSDictionary *attrs = @{ NSFontAttributeName: [NSFont systemFontOfSize:11],
                                 NSForegroundColorAttributeName: [NSColor colorWithCalibratedWhite:1 alpha:0.8] };

        // The capsule is 227pt wide, centred, hanging from the top edge.
        CGFloat bodyW = 227 * scale, bodyH = 34 * scale;
        CGFloat x0 = pad + (panelW - bodyW) / 2;
        NSRect frame1 = NSMakeRect(pad, 26, panelW, panelH);
        NSRect shape1 = NSMakeRect(x0, NSMinY(frame1), bodyW, bodyH);
        panel(frame1, shape1, 8, 12, YES, scale, labels[0], attrs, height, 0, 0);

        NSRect frame2 = NSMakeRect(pad, NSMaxY(frame1) + 34, panelW, panelH);
        NSRect shape2 = NSMakeRect(x0, NSMinY(frame2), bodyW, bodyH);
        panel(frame2, shape2, 9 * 0.60, 12, YES, scale, labels[1], attrs, height, 0.30, 9 * 0.60);   // breath, low
        NSRect frame3 = NSMakeRect(pad, NSMaxY(frame2) + 34, panelW, panelH);
        NSRect shape3 = NSMakeRect(x0, NSMinY(frame3), bodyW, bodyH);
        panel(frame3, shape3, 9 * 1.35, 12, YES, scale, labels[2], attrs, height, 0.75, 9 * 1.35);   // breath, peak

        [NSGraphicsContext restoreGraphicsState];
        NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        [png writeToFile:@"/tmp/barproto/icons/shape.png" atomically:YES];
        printf("wrote shape.png\n");
    }
    return 0;
}
